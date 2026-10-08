// DevShare for iPhone and iPad: join a session with its invitation and use
// its services in the app's own browser, under their real names.

import SwiftUI

@main
struct DevShareApp: App {
    @State private var model = SessionModel()

    var body: some Scene {
        WindowGroup {
            Group {
                if let session = model.session {
                    SessionView(session: session)
                } else {
                    JoinView()
                }
            }
            .environment(model)
            // `devshare://open?link=…`, from an invitation page: shown, never
            // joined without a tap.
            .onOpenURL { url in model.handOver(url) }
        }
    }
}
