import SwiftUI

struct AddDevice: View {
    @ObservedObject var model: BridgeModel
    @State private var product = "dyson-hp09"
    @State private var email = ""
    @State private var country = "GB"
    @State private var password = ""
    @State private var code = ""
    @State private var codeSent = false
    @State private var name = "The Frame"
    @State private var host = ""
    @State private var error: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                Text("Add a device")
                    .font(.system(size: 15, weight: .semibold))
                Spacer()
                Button("Close") { model.adding = false }
                    .buttonStyle(.borderless)
                    .font(.system(size: 12))
                    .foregroundStyle(.secondary)
            }
            Picker("Product", selection: $product) {
                ForEach(model.products) { item in
                    Text(item.name).tag(item.id)
                }
            }
            .labelsHidden()
            if product == "frame-2024" {
                TextField("Name in Home", text: $name)
                TextField("IP address, if it is not found", text: $host)
                Text("Turn the TV on. If the screen asks, press Allow.")
                    .font(.system(size: 12))
                    .foregroundStyle(.secondary)
                Button(model.busy ? "Waiting for the TV" : "Pair") {
                    model.pairFrame(name: name, host: host) { error = $0 }
                }
                .disabled(model.busy || name.isEmpty)
            } else {
                TextField("MyDyson email", text: $email)
                TextField("Country", text: $country)
                SecureField("Password", text: $password)
                if codeSent {
                    TextField("Email code", text: $code)
                    Button(model.busy ? "Checking" : "Add fan") {
                        model.submitDyson(password: password, code: code) { error = $0 }
                    }
                    .disabled(model.busy || code.isEmpty || password.isEmpty)
                } else {
                    Button(model.busy ? "Sending" : "Email me a code") {
                        model.requestDyson(email: email, country: country) { message in
                            codeSent = message == nil
                            error = message
                        }
                    }
                    .disabled(model.busy || email.isEmpty)
                }
            }
            if let error, !error.isEmpty {
                Text(error)
                    .font(.system(size: 12))
                    .foregroundStyle(.red)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .textFieldStyle(.roundedBorder)
    }
}
