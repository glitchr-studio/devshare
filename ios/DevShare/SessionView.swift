import SwiftUI

struct SessionView: View {
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
                            if let url = URL(string: service.url) {
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
