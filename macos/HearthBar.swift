import SwiftUI

@main
struct HearthBar: App {
    @StateObject private var model = BridgeModel()

    var body: some Scene {
        MenuBarExtra {
            Panel(model: model)
        } label: {
            Image(systemName: "flame")
        }
        .menuBarExtraStyle(.window)
    }
}
