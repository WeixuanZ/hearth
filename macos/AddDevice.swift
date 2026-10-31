import AppKit
import SwiftUI

/// A device that can be added, from `{"op":"products"}`.
struct Product: Decodable, Identifiable, Equatable {
    var id: String
    var name: String
    var category: String
}

/// One step of adding a device, from `{"op":"setup"}`. Drawn as a form,
/// whatever the device: the connector decides what it asks.
struct SetupStep: Decodable, Equatable {
    var title: String
    var note: String
    var fields: [SetupField]?
    var link: String?
    /// The field the page at `link` fills, by sending the browser back to
    /// an address this app answers.
    var callback: String?
    var wait: Bool?
}

struct SetupField: Decodable, Identifiable, Equatable {
    var id: String
    var label: String
    /// text, secret or choice.
    var kind: String
    var options: [Option]?
    var value: String?
    var hint: String?
    var optional: Bool
}

private struct SetupReply: Decodable {
    var step: SetupStep?
    var done: String?
}

/// Adding a device: kept by the app rather than the panel, so a sign-in
/// that sends the browser back here finds it, even with the panel closed.
@MainActor
final class SetupSession: ObservableObject {
    @Published var active = false
    @Published private(set) var products: [Product] = []
    @Published private(set) var product: Product?
    @Published private(set) var step: SetupStep?
    @Published var answers: [String: String] = [:]
    @Published private(set) var busy = false
    @Published private(set) var failure: String?
    @Published private(set) var done: String?

    func open() {
        close()
        active = true
        Task { await loadProducts() }
    }

    func close() {
        active = false
        product = nil
        step = nil
        answers = [:]
        failure = nil
        done = nil
    }

    func start(_ product: Product) async {
        self.product = product
        await send(["op": "setup", "product": product.id])
    }

    func advance() async {
        guard let product else { return }
        await send(["op": "setup", "product": product.id, "answers": answers])
    }

    /// An address a sign-in page sent the browser to. It fills the field the
    /// step waits on, and the setup goes on. Anything else is not ours.
    func receive(_ url: URL) {
        guard let field = step?.callback, !busy else { return }
        answers[field] = url.absoluteString
        Task { await advance() }
    }

    private func loadProducts() async {
        guard let data = try? await Task.detached(operation: { try ControlSocket.request(["op": "products"]) }).value,
              let list = try? JSONDecoder().decode([Product].self, from: data)
        else {
            failure = "Start the bridge to add a device."
            return
        }
        products = list
    }

    private func send(_ body: [String: Any]) async {
        busy = true
        failure = nil
        defer { busy = false }
        do {
            let data = try await Task.detached { try ControlSocket.request(body, wait: 180) }.value
            let reply = try JSONDecoder().decode(SetupReply.self, from: data)
            if let next = reply.step {
                answers = Dictionary(uniqueKeysWithValues: (next.fields ?? []).compactMap { field in
                    (field.value ?? field.options?.first?.id).map { (field.id, $0) }
                })
                step = next
            } else if let message = reply.done {
                done = message
            }
        } catch {
            failure = (error as? ControlSocket.Failure)?.message ?? error.localizedDescription
        }
    }
}

/// Adding a device, in the panel: choose what to add, then its steps.
struct AddDevice: View {
    @ObservedObject var session: SetupSession

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack {
                Text(session.product.map { "Add \($0.name)" } ?? "Add a device")
                    .font(.system(size: 13, weight: .semibold))
                Spacer()
                Button(session.done == nil ? "Cancel" : "Done") { session.close() }
                    .buttonStyle(.borderless)
                    .font(.system(size: 12))
            }
            if let done = session.done {
                Label(done, systemImage: "checkmark.circle.fill")
                    .font(.system(size: 12))
                    .foregroundStyle(.green, .primary)
                    .fixedSize(horizontal: false, vertical: true)
            } else if let step = session.step {
                form(step)
            } else {
                choices
            }
            if let failure = session.failure {
                Label(failure, systemImage: "exclamationmark.triangle")
                    .font(.system(size: 11))
                    .foregroundStyle(.red)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(
            RoundedRectangle(cornerRadius: 10, style: .continuous)
                .fill(Color.primary.opacity(0.045))
        )
    }

    private var choices: some View {
        VStack(spacing: 6) {
            ForEach(session.products) { product in
                Button {
                    Task { await session.start(product) }
                } label: {
                    HStack(spacing: 8) {
                        Image(systemName: categorySymbol(product.category))
                            .foregroundStyle(.secondary)
                            .frame(width: 16)
                        Text(product.name)
                        Spacer()
                        Image(systemName: "chevron.right")
                            .font(.system(size: 10, weight: .semibold))
                            .foregroundStyle(.tertiary)
                    }
                    .font(.system(size: 12))
                    .padding(.vertical, 7)
                    .padding(.horizontal, 8)
                    .background(
                        RoundedRectangle(cornerRadius: 7, style: .continuous)
                            .fill(Color.primary.opacity(0.05))
                    )
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .disabled(session.busy)
            }
        }
    }

    private func form(_ step: SetupStep) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            Text(step.title)
                .font(.system(size: 12, weight: .semibold))
            if !step.note.isEmpty {
                Text(step.note)
                    .font(.system(size: 11))
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            if let link = step.link, let url = URL(string: link) {
                Button {
                    NSWorkspace.shared.open(url)
                } label: {
                    Label("Open the sign-in page", systemImage: "safari")
                }
                .controlSize(.small)
            }
            ForEach(fields(step)) { field in
                VStack(alignment: .leading, spacing: 3) {
                    Text(field.label)
                        .font(.system(size: 11))
                        .foregroundStyle(.secondary)
                    input(field)
                }
            }
            if let field = step.fields?.first(where: { $0.id == step.callback }) {
                DisclosureGroup("Paste the address instead") {
                    input(field).padding(.top, 4)
                }
                .font(.system(size: 11))
            }
            HStack(spacing: 8) {
                if session.busy {
                    ProgressView().controlSize(.small)
                    Text(step.wait == true ? "Waiting…" : "Working…")
                        .font(.system(size: 11))
                        .foregroundStyle(.secondary)
                } else if step.callback != nil {
                    Text("Waiting for the sign-in…")
                        .font(.system(size: 11))
                        .foregroundStyle(.secondary)
                }
                Spacer()
                if step.wait != true || session.failure != nil {
                    Button(step.wait == true ? "Try again" : "Continue") {
                        Task { await session.advance() }
                    }
                    .buttonStyle(.borderedProminent)
                    .controlSize(.small)
                    .keyboardShortcut(.defaultAction)
                    .disabled(session.busy || !complete(step))
                }
            }
        }
        .textFieldStyle(.roundedBorder)
        // A step that waits on the device goes on by itself.
        .task(id: step) {
            if step.wait == true { await session.advance() }
        }
    }

    /// The fields to show. The one a sign-in fills by itself is offered
    /// folded away, for a browser that will not open the app.
    private func fields(_ step: SetupStep) -> [SetupField] {
        (step.fields ?? []).filter { $0.id != step.callback }
    }

    @ViewBuilder
    private func input(_ field: SetupField) -> some View {
        let text = Binding(get: { session.answers[field.id] ?? "" }, set: { session.answers[field.id] = $0 })
        switch field.kind {
        case "secret":
            SecureField(field.hint ?? "", text: text)
        case "choice":
            Picker(field.label, selection: text) {
                ForEach(field.options ?? []) { Text($0.name).tag($0.id) }
            }
            .labelsHidden()
        default:
            TextField(field.hint ?? (field.optional ? "Optional" : ""), text: text)
        }
    }

    private func complete(_ step: SetupStep) -> Bool {
        (step.fields ?? []).allSatisfy { field in
            field.optional || field.id == step.callback
                || !(session.answers[field.id] ?? "").trimmingCharacters(in: .whitespaces).isEmpty
        }
    }
}
