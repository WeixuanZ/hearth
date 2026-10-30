import SwiftUI

/// The computers sharing the bridge, when there is something to do or know:
/// this one asking to join, another asking to join this one, or another
/// group on the network.
struct GroupCard: View {
    @ObservedObject var model: BridgeModel
    let group: GroupStatus
    @State private var typed = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            if let code = group.code {
                joining(code)
            } else if let asking = group.asking {
                approving(asking)
            } else if let other = group.other {
                otherGroup(other)
            } else {
                members
            }
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(
            RoundedRectangle(cornerRadius: 10, style: .continuous)
                .fill(Color.primary.opacity(0.045))
        )
    }

    static func shows(_ group: GroupStatus) -> Bool {
        group.code != nil || group.asking != nil || group.other != nil || !group.members.isEmpty
    }

    private func joining(_ code: String) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            title("person.2", "Join \(group.leader ?? "the other Mac")")
            Text(code)
                .font(.system(size: 24, weight: .semibold, design: .monospaced))
                .textSelection(.enabled)
                .padding(.leading, 24)
            note("Type this code in Hearth on \(group.leader ?? "the other Mac"), then confirm here. Confirm only if you typed it there.")
            HStack(spacing: 8) {
                Button("Stay separate") { model.group("separate") }
                Spacer()
                Button("I typed it") { model.group("confirm") }
                    .buttonStyle(.borderedProminent)
            }
            .controlSize(.small)
        }
    }

    private func approving(_ asking: String) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            title("person.badge.plus", "\(asking) wants to join")
            note("It will share this bridge and its devices. Type the code it shows.")
            TextField("Code", text: $typed)
                .textFieldStyle(.roundedBorder)
                .font(.system(size: 13, design: .monospaced))
                .onSubmit(approve)
                .padding(.leading, 24)
            HStack(spacing: 8) {
                Button("Decline") { model.group("decline") }
                Spacer()
                Button("Approve", action: approve)
                    .buttonStyle(.borderedProminent)
                    .disabled(typed.filter(\.isNumber).count != 6)
            }
            .controlSize(.small)
        }
    }

    private func approve() {
        model.group("approve", code: typed)
        typed = ""
    }

    private func otherGroup(_ other: String) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            title("person.2.slash", "\(other) runs its own Hearth")
            note("Join it to share one bridge. That Mac will ask for a code.")
            HStack {
                Spacer()
                Button("Join it") { model.group("join") }
                    .controlSize(.small)
            }
        }
    }

    private var members: some View {
        VStack(alignment: .leading, spacing: 4) {
            title("person.2", "Shared with \(group.members.joined(separator: ", "))")
            note(group.serving
                ? "This Mac serves Home. Another takes over if it sleeps."
                : "\(group.leader ?? "Another Mac") serves Home. This Mac takes over if it sleeps.")
        }
    }

    private func title(_ symbol: String, _ text: String) -> some View {
        HStack(spacing: 8) {
            Image(systemName: symbol)
                .font(.system(size: 12, weight: .medium))
                .foregroundStyle(.secondary)
                .frame(width: 16)
            Text(text)
                .font(.system(size: 13, weight: .semibold))
                .lineLimit(2)
        }
    }

    private func note(_ text: String) -> some View {
        Text(text)
            .font(.system(size: 12))
            .foregroundStyle(.secondary)
            .fixedSize(horizontal: false, vertical: true)
            .padding(.leading, 24)
    }
}
