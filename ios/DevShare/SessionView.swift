import SwiftUI

struct SessionView: View {
    /// What a launch's kind is called for the person: the provider's app.
    static func title(of launch: Launch) -> String {
        switch launch.kind {
        case "expo": return "Open in Expo Go"
        default: return "Open with \(launch.kind)"
        }
    }

    @Environment(SessionModel.self) private var model
    let session: GuestSession
    @State private var leaving = false

    var body: some View {
        NavigationStack {
            List {
                Section {
                    LabeledContent("Time left", value: model.remaining.clock)
                        .monospacedDigit()
                    LabeledContent("Link", value: model.route ?? "connecting…")
                }
                ForEach(model.environments, id: \.name) { environment in
                    Section(environment.name) {
                        if let entrypoint = environment.entrypoint, let url = URL(string: entrypoint) {
                            NavigationLink(value: url) {
                                Label("Open \(entrypoint)", systemImage: "safari")
                            }
                        }
                        ForEach(environment.services, id: \.address) { service in
                            if service.kind != nil {
                                // Not the web: for another app than the browser.
                                HStack {
                                    Text(service.address).font(.body.monospaced())
                                    Spacer()
                                    Text(service.kind ?? "").font(.caption2).foregroundStyle(.secondary)
                                }
                            } else if let url = URL(string: service.url) {
                                NavigationLink(value: url) {
                                    HStack {
                                        Text(service.address).font(.body.monospaced())
                                        Spacer()
                                        if service.tls {
                                            Text("HTTPS").font(.caption2).foregroundStyle(.secondary)
                                        }
                                    }
                                }
                            }
                        }
                        // Ways to open it with another app. This app has no
                        // provider yet: the other app would need the whole
                        // phone in the session, which the tunnel extension
                        // will give.
                        ForEach(environment.launches, id: \.url) { launch in
                            VStack(alignment: .leading, spacing: 2) {
                                Label(Self.title(of: launch), systemImage: "arrow.up.forward.app")
                                    .foregroundStyle(.secondary)
                                Text("\(launch.url) — needs DevShare's tunnel on this phone, not yet available")
                                    .font(.caption2).foregroundStyle(.secondary)
                            }
                        }
                    }
                }
            }
            .navigationTitle("In a session")
            .navigationDestination(for: URL.self) { url in
                BrowserView(session: session, url: url)
                    .navigationTitle(url.host() ?? "")
                    .navigationBarTitleDisplayMode(.inline)
            }
            .toolbar {
                Button("Leave", role: .destructive) {
                    leaving = true
                    Task { await model.leave() }
                }
                .disabled(leaving)
            }
        }
    }
}
