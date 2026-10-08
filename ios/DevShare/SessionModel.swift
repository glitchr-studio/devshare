import Foundation
import Observation
import UIKit

/// The session this device is in, or the invitation it is about to use.
@MainActor
@Observable
final class SessionModel {
    var invitation = ""
    /// The invitation came from a link, not from the person holding the
    /// phone: they are told before they join.
    var handed = false
    var joining = false
    var problem: String?
    /// Why the last session ended.
    var ended: String?

    private(set) var session: GuestSession?
    private(set) var environments: [SharedEnvironment] = []
    private(set) var remaining: UInt64 = 0
    private(set) var route: String?
    private var ticker: Task<Void, Never>?

    func handOver(_ url: URL) {
        guard session == nil, url.scheme == "devshare",
              let link = URLComponents(url: url, resolvingAgainstBaseURL: false)?
                  .queryItems?.first(where: { $0.name == "link" })?.value,
              !link.isEmpty, link.count <= 2048 else { return }
        invitation = link
        handed = true
        problem = nil
    }

    func join() async {
        let invitation = invitation.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !invitation.isEmpty, !joining else { return }
        joining = true
        problem = nil
        defer { joining = false }
        do {
            let folder = try FileManager.default
                .url(for: .applicationSupportDirectory, in: .userDomainMask, appropriateFor: nil, create: true)
                .appendingPathComponent("devshare").path
            let session = try await DevShare.join(
                invitation: invitation, deviceName: UIDevice.current.name, dataFolder: folder)
            self.session = session
            environments = session.environments()
            remaining = session.remainingSeconds()
            self.invitation = ""
            handed = false
            ended = nil
            follow(session)
        } catch MobileError.Refused(let reason) {
            problem = reason
        } catch {
            problem = error.localizedDescription
        }
    }

    func leave() async {
        await session?.leave()
    }

    /// The countdown, the route, and the end of the session.
    private func follow(_ session: GuestSession) {
        ticker?.cancel()
        ticker = Task { [weak self] in
            while !Task.isCancelled {
                guard let self else { return }
                if let reason = session.ended() {
                    self.ended = reason
                    self.session = nil
                    self.environments = []
                    self.route = nil
                    return
                }
                self.remaining = session.remainingSeconds()
                self.route = session.route()
                try? await Task.sleep(for: .seconds(1))
            }
        }
    }
}

extension UInt64 {
    /// `04:23`, or no time limit for a session that runs until its host
    /// stops it.
    var clock: String {
        if self > 5 * 365 * 24 * 3600 { return "no time limit" }
        return String(format: "%02d:%02d", self / 60, self % 60)
    }
}
