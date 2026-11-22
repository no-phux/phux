import UIKit
import VisionKit
import AVFoundation

/// The camera only returns text. The shared pairing parser decides whether
/// it is a Phux credential, and the person still confirms before enrolling.
@MainActor
final class PairingScanner: UIViewController, DataScannerViewControllerDelegate {
    private static let instruction = "On your computer, run \(pairCommand) and point the camera at its QR code."
    private static let wrongCode = "That isn't a Phux pairing code. Scan the QR code that \(pairCommand) shows."

    var received: ((String) -> Void)?
    var failed: ((String) -> Void)?
    private let scanner = DataScannerViewController(
        recognizedDataTypes: [.barcode(symbologies: [.qr])], qualityLevel: .balanced,
        recognizesMultipleItems: false, isHighFrameRateTrackingEnabled: false,
        isPinchToZoomEnabled: true, isGuidanceEnabled: true, isHighlightingEnabled: true)
    private let caption = UILabel()
    private var captionReset: Task<Void, Never>?
    private var consumed = false
    private let torch = AVCaptureDevice.default(for: .video).flatMap { $0.hasTorch ? $0 : nil }
    isolated deinit { captionReset?.cancel() }

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
        overrideUserInterfaceStyle = .dark
        view.backgroundColor = .black // Under the bar, where the preview does not reach.
        navigationItem.leftBarButtonItem = UIBarButtonItem(systemItem: .cancel,
            primaryAction: UIAction { [weak self] _ in self?.dismiss(animated: true) })
        if torch != nil {
            navigationItem.rightBarButtonItem = UIBarButtonItem(primaryAction: UIAction { [weak self] _ in self?.toggleTorch() })
            showTorch(on: false)
        }
        scanner.delegate = self
        addChild(scanner)
        scanner.view.translatesAutoresizingMaskIntoConstraints = false
        view.addSubview(scanner.view)
        let plate = captionPlate()
        view.addSubview(plate)
        NSLayoutConstraint.activate([
            scanner.view.topAnchor.constraint(equalTo: view.safeAreaLayoutGuide.topAnchor),
            scanner.view.bottomAnchor.constraint(equalTo: view.bottomAnchor),
            scanner.view.leadingAnchor.constraint(equalTo: view.leadingAnchor),
            scanner.view.trailingAnchor.constraint(equalTo: view.trailingAnchor),
            plate.leadingAnchor.constraint(equalTo: view.layoutMarginsGuide.leadingAnchor),
            plate.trailingAnchor.constraint(equalTo: view.layoutMarginsGuide.trailingAnchor),
            plate.bottomAnchor.constraint(equalTo: view.safeAreaLayoutGuide.bottomAnchor, constant: -16)])
        scanner.didMove(toParent: self)
    }

    /// Always on screen: the instruction cannot wait for a wrong code, and a
    /// navigation prompt would move the bar under the person's thumb.
    private func captionPlate() -> UIView {
        caption.text = Self.instruction
        caption.font = .preferredFont(forTextStyle: .callout)
        caption.adjustsFontForContentSizeCategory = true
        caption.textColor = .white
        caption.textAlignment = .center
        caption.numberOfLines = 0
        caption.accessibilityIdentifier = "scan-instruction"
        caption.translatesAutoresizingMaskIntoConstraints = false
        let plate = UIVisualEffectView(effect: UIBlurEffect(style: .systemMaterialDark))
        plate.layer.cornerRadius = 16
        plate.layer.cornerCurve = .continuous
        plate.clipsToBounds = true
        plate.translatesAutoresizingMaskIntoConstraints = false
        plate.contentView.addSubview(caption)
        NSLayoutConstraint.activate([
            caption.topAnchor.constraint(equalTo: plate.contentView.topAnchor, constant: 12),
            caption.bottomAnchor.constraint(equalTo: plate.contentView.bottomAnchor, constant: -12),
            caption.leadingAnchor.constraint(equalTo: plate.contentView.leadingAnchor, constant: 16),
            caption.trailingAnchor.constraint(equalTo: plate.contentView.trailingAnchor, constant: -16)])
        return plate
    }

    override func viewDidAppear(_ animated: Bool) {
        super.viewDidAppear(animated)
        do { try scanner.startScanning() }
        catch { finish(error: "The camera could not start. Use Paste pairing link or try again.") }
    }

    override func viewWillDisappear(_ animated: Bool) {
        setTorch(on: false)
        scanner.stopScanning()
        super.viewWillDisappear(animated)
    }

    private func toggleTorch() { setTorch(on: torch?.torchMode != .on) }

    private func setTorch(on: Bool) {
        guard let torch, torch.isTorchAvailable, torch.isTorchModeSupported(on ? .on : .off),
              (try? torch.lockForConfiguration()) != nil else { return }
        torch.torchMode = on ? .on : .off
        torch.unlockForConfiguration()
        showTorch(on: on)
    }

    private func showTorch(on: Bool) {
        let button = navigationItem.rightBarButtonItem
        button?.image = UIImage(systemName: on ? "flashlight.on.fill" : "flashlight.off.fill")
        button?.accessibilityLabel = "Flashlight"
        button?.accessibilityValue = on ? "On" : "Off"
    }

    func dataScanner(_ dataScanner: DataScannerViewController, didAdd addedItems: [RecognizedItem],
                     allItems: [RecognizedItem]) {
        guard !consumed, let item = addedItems.first,
              case .barcode(let barcode) = item, let text = barcode.payloadStringValue else { return }
        do { _ = try PairingInvite(link: text) }
        catch {
            rejectCode()
            return
        }
        consumed = true
        scanner.stopScanning()
        UINotificationFeedbackGenerator().notificationOccurred(.success)
        let receive = received
        dismiss(animated: true) { receive?(text) }
    }

    private func rejectCode() {
        caption.text = Self.wrongCode
        UIAccessibility.post(notification: .announcement, argument: Self.wrongCode)
        captionReset?.cancel()
        captionReset = Task { [weak self] in
            try? await Task.sleep(for: .seconds(4))
            guard !Task.isCancelled else { return }
            self?.caption.text = Self.instruction
        }
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
