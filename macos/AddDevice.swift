import AppKit
import SwiftUI

/// A device that can be added, from `{"op":"products"}`.
struct Product: Decodable, Identifiable {
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

/// Adding a device, in the panel: choose what to add, then its steps.
struct AddDevice: View {
    @ObservedObject var model: BridgeModel
    var close: () -> Void
    @State private var products: [Product] = []
    @State private var product: Product?
    @State private var step: SetupStep?
    @State private var answers: [String: String] = [:]
    @State private var busy = false
    @State private var failure: String?
    @State private var done: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack {
                Text(product.map { "Add \($0.name)" } ?? "Add a device")
                    .font(.system(size: 13, weight: .semibold))
                Spacer()
                Button(done == nil ? "Cancel" : "Done", action: close)
                    .buttonStyle(.borderless)
                    .font(.system(size: 12))
            }
            if let done {
                Label(done, systemImage: "checkmark.circle.fill")
                    .font(.system(size: 12))
                    .foregroundStyle(.green, .primary)
                    .fixedSize(horizontal: false, vertical: true)
            } else if let step {
                form(step)
            } else {
                choices
            }
            if let failure {
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
        .task { await loadProducts() }
    }

    private var choices: some View {
        VStack(spacing: 6) {
            ForEach(products) { product in
                Button {
                    Task { await start(product) }
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
                .disabled(busy)
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
            ForEach(step.fields ?? []) { field in
                VStack(alignment: .leading, spacing: 3) {
                    Text(field.label)
                        .font(.system(size: 11))
                        .foregroundStyle(.secondary)
                    input(field)
                }
            }
            HStack(spacing: 8) {
                if step.wait == true, busy {
                    ProgressView().controlSize(.small)
                    Text("Waiting…")
                        .font(.system(size: 11))
                        .foregroundStyle(.secondary)
                }
                Spacer()
                if step.wait != true || failure != nil {
                    Button(step.wait == true ? "Try again" : "Continue") {
                        Task { await advance() }
                    }
                    .buttonStyle(.borderedProminent)
                    .controlSize(.small)
                    .keyboardShortcut(.defaultAction)
                    .disabled(busy || !complete(step))
                }
            }
        }
        .textFieldStyle(.roundedBorder)
        // A step that waits on the device goes on by itself.
        .task(id: step) {
            if step.wait == true { await advance() }
        }
    }

    @ViewBuilder
    private func input(_ field: SetupField) -> some View {
        let text = Binding(get: { answers[field.id] ?? "" }, set: { answers[field.id] = $0 })
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
        (step.fields ?? []).allSatisfy { $0.optional || !(answers[$0.id] ?? "").trimmingCharacters(in: .whitespaces).isEmpty }
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

    private func start(_ product: Product) async {
        self.product = product
        await send(["op": "setup", "product": product.id])
    }

    private func advance() async {
        guard let product else { return }
        await send(["op": "setup", "product": product.id, "answers": answers])
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
                model.refresh()
            }
        } catch {
            failure = (error as? ControlSocket.Failure)?.message ?? error.localizedDescription
        }
    }
}
