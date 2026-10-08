import Network
import SwiftUI
import WebKit

/// The app's browser: everything it loads goes through the session's proxy
/// on this phone's loopback. The session's own names are reached through
/// the session, and their HTTPS certificate must be exactly the one the host
/// saw when it shared them, whoever issued it; any other site is reached
/// directly and checked as usual.
struct BrowserView: UIViewRepresentable {
    let session: GuestSession
    let url: URL

    func makeCoordinator() -> Coordinator { Coordinator(session: session) }

    func makeUIView(context: Context) -> WKWebView {
        let store = WKWebsiteDataStore.nonPersistent()
        if let port = NWEndpoint.Port(rawValue: session.proxyPort()) {
            let proxy = ProxyConfiguration(httpCONNECTProxy: .hostPort(host: "127.0.0.1", port: port))
            proxy.applyCredential(username: session.proxyUser(), password: session.proxyPassword())
            store.proxyConfigurations = [proxy]
        }
        let configuration = WKWebViewConfiguration()
        configuration.websiteDataStore = store
        let view = WKWebView(frame: .zero, configuration: configuration)
        view.navigationDelegate = context.coordinator
        view.allowsBackForwardNavigationGestures = true
        view.load(URLRequest(url: url))
        return view
    }

    func updateUIView(_ view: WKWebView, context: Context) {}

    final class Coordinator: NSObject, WKNavigationDelegate {
        let session: GuestSession

        init(session: GuestSession) { self.session = session }

        func webView(_ webView: WKWebView, decidePolicyFor action: WKNavigationAction) async -> WKNavigationActionPolicy {
            guard let url = action.request.url, let scheme = url.scheme?.lowercased() else { return .cancel }
            if ["http", "https", "about", "blob", "data"].contains(scheme) { return .allow }
            // mailto:, tel: and the like belong to other apps.
            await UIApplication.shared.open(url)
            return .cancel
        }

        func webView(_ webView: WKWebView, respondTo challenge: URLAuthenticationChallenge) async -> (URLSession.AuthChallengeDisposition, URLCredential?) {
            let space = challenge.protectionSpace
            guard space.authenticationMethod == NSURLAuthenticationMethodServerTrust,
                  let trust = space.serverTrust else {
                return (.performDefaultHandling, nil)
            }
            // Another site: the system decides, as in any browser.
            guard session.isShared(host: space.host) else {
                return (.performDefaultHandling, nil)
            }
            guard let chain = SecTrustCopyCertificateChain(trust) as? [SecCertificate],
                  let leaf = chain.first,
                  session.certificateMatches(
                      host: space.host, port: UInt16(clamping: space.port),
                      certificate: SecCertificateCopyData(leaf) as Data) else {
                return (.cancelAuthenticationChallenge, nil)
            }
            return (.useCredential, URLCredential(trust: trust))
        }
    }
}
