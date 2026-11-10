import UIKit
import VisionKit
import AVFoundation

/// The camera only returns text. The shared pairing parser decides whether
/// it is a Phux credential, and the person still confirms before enrolling.
@MainActor
final class PairingScanner: UIViewController, DataScannerViewControllerDelegate {
    var received: ((String) -> Void)?
    var failed: ((String) -> Void)?
    private let scanner = DataScannerViewController(
        recognizedDataTypes: [.barcode(symbologies: [.qr])], qualityLevel: .balanced,
        recognizesMultipleItems: false, isHighFrameRateTrackingEnabled: false,
        isPinchToZoomEnabled: true, isGuidanceEnabled: true, isHighlightingEnabled: true)
    private var consumed = false

    static var needsSettings: Bool { AVCaptureDevice.authorizationStatus(for: .video) == .denied }

    static func cameraReady() async throws {
        guard DataScannerViewController.isSupported else {
            throw NativeFailure.message("This device cannot scan QR codes. Use Paste pairing link instead.")
        }
        let granted = await AVCaptureDevice.requestAccess(for: .video)
        guard granted else {
            throw NativeFailure.message("Allow camera access in Settings to scan, or use Paste pairing link.")
        }
        guard DataScannerViewController.isAvailable else {
            throw NativeFailure.message("The camera is unavailable right now. Use Paste pairing link or try again.")
        }
    }

    override func viewDidLoad() {
        super.viewDidLoad()
        title = "Scan pairing code"
        navigationItem.leftBarButtonItem = UIBarButtonItem(systemItem: .cancel,
            primaryAction: UIAction { [weak self] _ in self?.dismiss(animated: true) })
        scanner.delegate = self
        addChild(scanner)
        scanner.view.translatesAutoresizingMaskIntoConstraints = false
        view.addSubview(scanner.view)
        NSLayoutConstraint.activate([
            scanner.view.topAnchor.constraint(equalTo: view.safeAreaLayoutGuide.topAnchor),
            scanner.view.bottomAnchor.constraint(equalTo: view.bottomAnchor),
            scanner.view.leadingAnchor.constraint(equalTo: view.leadingAnchor),
            scanner.view.trailingAnchor.constraint(equalTo: view.trailingAnchor)])
        scanner.didMove(toParent: self)
    }

    override func viewDidAppear(_ animated: Bool) {
        super.viewDidAppear(animated)
        do { try scanner.startScanning() }
        catch { finish(error: "The camera could not start. Use Paste pairing link or try again.") }
    }

    override func viewWillDisappear(_ animated: Bool) {
        scanner.stopScanning()
        super.viewWillDisappear(animated)
    }

    func dataScanner(_ dataScanner: DataScannerViewController, didAdd addedItems: [RecognizedItem],
                     allItems: [RecognizedItem]) {
        guard !consumed, let item = addedItems.first,
              case .barcode(let barcode) = item, let text = barcode.payloadStringValue else { return }
        do { _ = try PairingInvite(link: text) }
        catch {
            navigationItem.prompt = "Scan the pairing QR from phux pair --enroll"
            return
        }
        consumed = true
        scanner.stopScanning()
        let receive = received
        dismiss(animated: true) { receive?(text) }
    }

    func dataScanner(_ dataScanner: DataScannerViewController,
                     becameUnavailableWithError error: DataScannerViewController.ScanningUnavailable) {
        finish(error: "The camera became unavailable. Use Paste pairing link or try again.")
    }

    private func finish(error: String) {
        guard !consumed else { return }
        consumed = true
        let failure = failed
        dismiss(animated: true) { failure?(error) }
    }
}
