import UIKit

#if targetEnvironment(simulator)
@MainActor
enum EarlierAppContractTests {
    private struct Failure: LocalizedError {
        let message: String
        var errorDescription: String? { message }
    }
    private static func require(_ value: @autoclosure () throws -> Bool, _ message: String) throws {
        if try !value() { throw Failure(message: message) }
    }

    static func run() throws -> [String] {
        try scenes()
        return ["earlier-app-scenes"]
    }

    /// A session another app's class would open goes; our own stays.
    private static func scenes() throws {
        let files = FileManager.default
        let library = files.temporaryDirectory.appending(path: UUID().uuidString)
        defer { try? files.removeItem(at: library) }
        let known = library.appending(path: "Saved Application State/app.savedState/KnownSceneSessions")
        func saved(by delegate: String) throws -> Bool {
            try files.createDirectory(at: known, withIntermediateDirectories: true)
            let data = try NSKeyedArchiver.archivedData(withRootObject: delegate as NSString, requiringSecureCoding: true)
            try data.write(to: known.appending(path: "data.data"))
            EarlierApp.forgetScenes(library: library, bundle: "app")
            return files.fileExists(atPath: known.path)
        }
        try require(try !saved(by: "SwiftUI.AppSceneDelegate"), "an earlier app's scene session must be forgotten")
        try require(try saved(by: NSStringFromClass(SceneDelegate.self)), "our own scene session must stay")
    }
}
#endif
