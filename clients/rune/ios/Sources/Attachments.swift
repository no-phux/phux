import AVFoundation
import PhotosUI
import UIKit
import UniformTypeIdentifiers

/// The paperclip's system menu and pickers. They run out of process and hand
/// over only what was picked, so nothing here asks for the photo library;
/// only the camera asks. Rune owns where each file goes (`attaching`).
@MainActor
final class Attachments: NSObject {
    /// Under the server's 64 MiB, so a phone's copies of it stay bounded.
    nonisolated static let maximumBytes = 48 * 1024 * 1024
    private static let maximumCount = 10

    /// One picked file, or nil when it could not be read.
    struct File: Sendable {
        let fileExtension: String
        let data: Data
    }
    /// Request, then each picked file in the order it was picked.
    var picked: ((UInt64, File?) -> Void)?
    /// A picker covers the surface, or no longer does.
    var covering: ((Bool) -> Void)?
    private var request: UInt64 = 0

    /// The menu's anchor: Rune draws the paperclip, UIKit opens from it.
    private let anchor = UIButton(type: .custom)

    /// Open the paperclip's menu from `source` in `host`.
    func show(request: UInt64, over host: UIView, at source: CGRect) {
        guard presenter(over: host) != nil else { return }
        self.request = request
        func item(_ title: String, _ symbol: String, _ run: @escaping (Attachments, UIViewController) -> Void) -> UIAction {
            UIAction(title: title, image: UIImage(systemName: symbol)) { [weak self, weak host] _ in
                if let self, let host, let presenter = self.presenter(over: host) { run(self, presenter) }
            }
        }
        var items = [
            item("Screenshots", "camera.viewfinder") { $0.photos(.any(of: [.screenshots, .screenRecordings]), from: $1) },
            item("Photo Library", "photo.on.rectangle") { $0.photos(.any(of: [.images, .videos]), from: $1) },
        ]
        if UIImagePickerController.isSourceTypeAvailable(.camera) {
            items.append(item("Take Photo", "camera") { $0.camera(from: $1) })
        }
        items.append(item("Choose File", "folder") { $0.files(from: $1) })
        anchor.menu = UIMenu(children: items)
        anchor.showsMenuAsPrimaryAction = true
        anchor.isAccessibilityElement = false
        anchor.frame = source
        // Under every control: it opens the menu and takes no touch itself.
        host.insertSubview(anchor, at: 0)
        anchor.performPrimaryAction()
    }

    private func presenter(over host: UIView) -> UIViewController? {
        guard let root = host.window?.rootViewController, root.presentedViewController == nil else { return nil }
        return root
    }

    private func photos(_ filter: PHPickerFilter, from presenter: UIViewController) {
        var configuration = PHPickerConfiguration()
        configuration.filter = filter
        configuration.selectionLimit = Self.maximumCount
        configuration.selection = .ordered
        // HEIC and HEVC arrive as JPEG and H.264, which agents can read.
        configuration.preferredAssetRepresentationMode = .compatible
        let picker = PHPickerViewController(configuration: configuration)
        picker.delegate = self
        present(picker, from: presenter)
    }

    private func camera(from presenter: UIViewController) {
        guard AVCaptureDevice.authorizationStatus(for: .video) != .denied else {
            let alert = UIAlertController(title: "Camera access is off",
                message: "Allow the camera in Settings to take a photo here.", preferredStyle: .alert)
            alert.addAction(UIAlertAction(title: "Cancel", style: .cancel))
            alert.addAction(UIAlertAction(title: "Open Settings", style: .default) { _ in
                if let url = URL(string: UIApplication.openSettingsURLString) { UIApplication.shared.open(url) }
            })
            return present(alert, from: presenter)
        }
        let picker = UIImagePickerController()
        picker.sourceType = .camera
        picker.delegate = self
        picker.modalPresentationStyle = .fullScreen
        present(picker, from: presenter)
    }

    private func files(from presenter: UIViewController) {
        // A copy: nothing here writes back, and a copy needs no scoped access.
        let picker = UIDocumentPickerViewController(forOpeningContentTypes: [.item], asCopy: true)
        picker.allowsMultipleSelection = true
        picker.delegate = self
        present(picker, from: presenter)
    }

    private func present(_ picker: UIViewController, from presenter: UIViewController) {
        covering?(true)
        presenter.present(picker, animated: true)
        // Each picker reports picks, cancels and swipes differently, and the
        // document picker dismisses itself: wait for what they all end in.
        // A cold one can take seconds to appear.
        Task { [weak self, weak picker] in
            let pause = Duration.milliseconds(100)
            for _ in 0..<100 where picker?.presentingViewController == nil { try? await Task.sleep(for: pause) }
            while picker?.presentingViewController != nil { try? await Task.sleep(for: pause) }
            self?.covering?(false)
        }
    }

    /// The pasteboard's image when it holds no text: a copied screenshot.
    /// Nil when the person declined, too; like text, that says nothing.
    static func pasted() -> File? {
        let board = UIPasteboard.general
        guard board.hasImages else { return nil }
        for type in [UTType.png, .jpeg] {
            if let data = board.data(forPasteboardType: type.identifier), data.count <= maximumBytes {
                return File(fileExtension: type == .png ? "png" : "jpg", data: data)
            }
        }
        return board.image?.jpegData(compressionQuality: 0.85).map { File(fileExtension: "jpg", data: $0) }
    }

    /// `data` as an agent can read it: PNG, JPEG, GIF, WebP and anything that
    /// isn't an image go byte for byte; other images (HEIC) become JPEG.
    nonisolated static func file(_ data: Data, named name: String) -> File? {
        guard !data.isEmpty, data.count <= maximumBytes else { return nil }
        let given = (name as NSString).pathExtension.lowercased()
        // What the server's upload folder accepts as an extension.
        let clean = (1...16).contains(given.utf8.count)
            && given.utf8.allSatisfy { (0x61...0x7a).contains($0) || (0x30...0x39).contains($0) }
        let type = clean ? UTType(filenameExtension: given) : nil
        let readable: [UTType] = [.png, .jpeg, .gif, .webP, .svg]
        if let type, type.conforms(to: .image), !readable.contains(where: type.conforms),
           let image = UIImage(data: data)?.jpegData(compressionQuality: 0.85), image.count <= maximumBytes {
            return File(fileExtension: "jpg", data: image)
        }
        return File(fileExtension: clean ? given : "bin", data: data)
    }

    /// Read without loading what the cap would refuse.
    nonisolated static func file(at url: URL) -> File? {
        guard let size = try? url.resourceValues(forKeys: [.fileSizeKey]).fileSize, size <= maximumBytes,
              let data = try? Data(contentsOf: url) else { return nil }
        return file(data, named: url.lastPathComponent)
    }

    /// A photo's temporary file lives only for the callback that names it.
    private static func load(_ provider: NSItemProvider) async -> File? {
        let types = provider.registeredTypeIdentifiers.compactMap { UTType($0) }
        let image = types.first { [.png, .jpeg, .gif, .webP].contains($0) } ?? types.first { $0.conforms(to: .image) }
        guard let type = image ?? types.first(where: { $0.conforms(to: .movie) }) else { return nil }
        return await withCheckedContinuation { continuation in
            _ = provider.loadFileRepresentation(forTypeIdentifier: type.identifier) { url, _ in
                continuation.resume(returning: url.flatMap(file(at:)))
            }
        }
    }
}

extension Attachments: PHPickerViewControllerDelegate, UIDocumentPickerDelegate, UIImagePickerControllerDelegate,
    UINavigationControllerDelegate {
    func picker(_ picker: PHPickerViewController, didFinishPicking results: [PHPickerResult]) {
        let request = request
        picker.dismiss(animated: true)
        Task {
            for result in results { picked?(request, await Self.load(result.itemProvider)) }
        }
    }

    func documentPicker(_ controller: UIDocumentPickerViewController, didPickDocumentsAt urls: [URL]) {
        let request = request
        Task {
            for url in urls.prefix(Self.maximumCount) {
                let file = await Task.detached { Self.file(at: url) }.value
                picked?(request, file)
            }
        }
    }

    func imagePickerController(_ picker: UIImagePickerController,
                               didFinishPickingMediaWithInfo info: [UIImagePickerController.InfoKey: Any]) {
        let photo = (info[.originalImage] as? UIImage)?.jpegData(compressionQuality: 0.85)
        let request = request
        picker.dismiss(animated: true) { [weak self] in
            self?.picked?(request, photo.flatMap { Self.file($0, named: "photo.jpg") })
        }
    }
    func imagePickerControllerDidCancel(_ picker: UIImagePickerController) { picker.dismiss(animated: true) }
}
