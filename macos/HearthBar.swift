import SwiftUI

@main
struct HearthBar: App {
    @StateObject private var model = BridgeModel()
    @StateObject private var updater = Updater()

    var body: some Scene {
        MenuBarExtra {
            Panel(model: model, updater: updater)
        } label: {
            Image(systemName: "flame")
        }
        .menuBarExtraStyle(.window)
    }
}
