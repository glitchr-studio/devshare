import SwiftUI
import VisionKit

struct JoinView: View {
    @Environment(SessionModel.self) private var model
    @State private var scanning = false

    var body: some View {
        @Bindable var model = model
        NavigationStack {
            Form {
                if let ended = model.ended {
                    Section {
                        Text("The session is over: \(ended).")
                            .foregroundStyle(.secondary)
                    }
                }
                Section {
                    TextField("Link or code", text: $model.invitation, axis: .vertical)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                        .font(.body.monospaced())
                        .onChange(of: model.invitation) { model.problem = nil }
                    HStack {
                        Button("Paste") {
                            if let text = UIPasteboard.general.string {
                                model.invitation = text
                                model.handed = false
                            }
                        }
                        Spacer()
                        if DataScannerViewController.isSupported {
                            Button("Scan a QR code") { scanning = true }
                        }
                    }
                    .buttonStyle(.borderless)
                } header: {
                    Text("Invitation")
                } footer: {
                    if model.handed {
                        Text("A link handed this invitation over. Join only if you know who sent it.")
                            .foregroundStyle(.orange)
                    }
                }

                Section {
                    Button {
                        Task { await model.join() }
                    } label: {
                        HStack {
                            Text(model.joining ? "Joining…" : "Join")
                            if model.joining { Spacer(); ProgressView() }
                        }
                    }
                    .disabled(model.joining || model.invitation.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                } footer: {
                    if let problem = model.problem {
                        Text(problem).foregroundStyle(.red).textSelection(.enabled)
                    }
                }

                Section {
                    Text("The shared services open in DevShare's own browser, under the names the host gave them. Your phone's other apps are not affected.")
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                }
            }
            .navigationTitle("DevShare")
            .sheet(isPresented: $scanning) {
                Scanner { payload in
                    scanning = false
                    model.invitation = payload
                    model.handed = false
                }
                .ignoresSafeArea()
            }
        }
    }
}

/// The camera, reading QR codes until one is found.
struct Scanner: UIViewControllerRepresentable {
    let found: (String) -> Void

    func makeUIViewController(context: Context) -> DataScannerViewController {
        let scanner = DataScannerViewController(
            recognizedDataTypes: [.barcode(symbologies: [.qr])],
            isHighlightingEnabled: true)
        scanner.delegate = context.coordinator
        try? scanner.startScanning()
        return scanner
    }

    func updateUIViewController(_ controller: DataScannerViewController, context: Context) {}

    func makeCoordinator() -> Coordinator { Coordinator(found: found) }

    final class Coordinator: NSObject, DataScannerViewControllerDelegate {
        let found: (String) -> Void
        private var done = false

        init(found: @escaping (String) -> Void) { self.found = found }

        func dataScanner(_ scanner: DataScannerViewController, didAdd items: [RecognizedItem], allItems: [RecognizedItem]) {
            for case .barcode(let code) in items {
                guard !done, let payload = code.payloadStringValue else { continue }
                done = true
                scanner.stopScanning()
                found(payload)
            }
        }
    }
}
