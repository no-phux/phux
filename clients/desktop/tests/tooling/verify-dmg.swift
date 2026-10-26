// Resolve Finder's actual alias after conversion, relocation and a fresh mount.
import AppKit

let mount = URL(fileURLWithPath: CommandLine.arguments[1])
let alias = try Data(contentsOf: URL(fileURLWithPath: CommandLine.arguments[2]))
let bookmark = CFURLCreateBookmarkDataFromAliasRecord(nil, alias as CFData)!.takeRetainedValue()
var stale = false
let resolved = try URL(resolvingBookmarkData: bookmark as Data,
    options: [.withoutUI, .withoutMounting], relativeTo: nil, bookmarkDataIsStale: &stale)
precondition(resolved.standardizedFileURL == mount.appendingPathComponent(".background.tiff").standardizedFileURL)
let background = NSImage(contentsOf: resolved)!
precondition(background.size == NSSize(width: 720, height: 460))
precondition(Set(background.representations.map { $0.pixelsWide }) == Set([720, 1440]))
