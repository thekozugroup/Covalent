import CryptoKit
import Darwin
import SwiftUI

private let noticeIndexName = "notices-index.txt"
private let maximumIndexBytes = 1_024
private let maximumNoticeBytes = 8 * 1_024 * 1_024
private let maximumManifestBytes = 4 * 1_024 * 1_024

private struct NoticeDescriptor {
    let relativePath: String
    let digest: String
    let bytes: Int
}

private struct NoticeIndex {
    let combined: NoticeDescriptor
    let manifest: NoticeDescriptor
}

enum MacOpenSourceNotices {
    static func load(bundle: Bundle = .main) throws -> String {
        guard let resources = bundle.resourceURL else { throw NoticeError.unavailable }
        return try load(from: resources.appendingPathComponent("CovalentSyncEngine", isDirectory: true))
    }

    static func load(from root: URL) throws -> String {
        try requireDirectory(root)
        try requireDirectory(root.appendingPathComponent("notices", isDirectory: true))
        let indexData = try read(root.appendingPathComponent(noticeIndexName), maximum: maximumIndexBytes)
        guard let indexText = String(data: indexData, encoding: .ascii) else {
            throw NoticeError.invalid
        }
        let index = try parse(indexText)
        let notice = try readVerified(root: root, descriptor: index.combined, maximum: maximumNoticeBytes)
        _ = try readVerified(root: root, descriptor: index.manifest, maximum: maximumManifestBytes)
        guard let text = String(data: notice, encoding: .utf8) else { throw NoticeError.invalid }
        return text
    }

    private static func requireDirectory(_ url: URL) throws {
        var metadata = stat()
        guard lstat(url.path, &metadata) == 0, (metadata.st_mode & S_IFMT) == S_IFDIR else {
            throw NoticeError.invalid
        }
    }

    private static func parse(_ text: String) throws -> NoticeIndex {
        guard !text.contains("\r"), text.hasSuffix("\n") else { throw NoticeError.invalid }
        let lines = text.dropLast().split(separator: "\n", omittingEmptySubsequences: false)
        guard lines.count == 3, lines[0] == "1" else { throw NoticeError.invalid }

        func descriptor(_ line: Substring, label: String, path: String, maximum: Int) throws -> NoticeDescriptor {
            let fields = line.split(separator: " ")
            guard fields.count == 4, fields[0] == Substring(label), fields[3] == Substring(path),
                  fields[1].count == 64, fields[1].allSatisfy({ $0.isHexDigit && !$0.isUppercase }),
                  let bytes = Int(fields[2]), bytes > 0, bytes <= maximum,
                  String(bytes) == fields[2] else {
                throw NoticeError.invalid
            }
            return NoticeDescriptor(relativePath: path, digest: String(fields[1]), bytes: bytes)
        }

        return try NoticeIndex(
            combined: descriptor(
                lines[1],
                label: "combined",
                path: "notices/THIRD-PARTY-NOTICES.txt",
                maximum: maximumNoticeBytes
            ),
            manifest: descriptor(
                lines[2],
                label: "manifest",
                path: "notices/manifest.json",
                maximum: maximumManifestBytes
            )
        )
    }

    private static func readVerified(
        root: URL,
        descriptor: NoticeDescriptor,
        maximum: Int
    ) throws -> Data {
        let data = try read(root.appendingPathComponent(descriptor.relativePath), maximum: maximum)
        guard data.count == descriptor.bytes,
              SHA256.hash(data: data).map({ String(format: "%02x", $0) }).joined() == descriptor.digest else {
            throw NoticeError.invalid
        }
        return data
    }

    private static func read(_ url: URL, maximum: Int) throws -> Data {
        let descriptor = open(url.path, O_RDONLY | O_CLOEXEC | O_NOFOLLOW | O_NONBLOCK)
        guard descriptor >= 0 else { throw NoticeError.unavailable }
        defer { close(descriptor) }
        var before = stat()
        guard fstat(descriptor, &before) == 0, (before.st_mode & S_IFMT) == S_IFREG,
              before.st_size > 0, before.st_size <= maximum else {
            throw NoticeError.invalid
        }
        var result = Data()
        result.reserveCapacity(Int(before.st_size))
        var buffer = [UInt8](repeating: 0, count: 32 * 1_024)
        while result.count <= maximum {
            let count = Darwin.read(descriptor, &buffer, min(buffer.count, maximum + 1 - result.count))
            if count == 0 { break }
            guard count > 0 else {
                if errno == EINTR { continue }
                throw NoticeError.unavailable
            }
            result.append(contentsOf: buffer.prefix(count))
        }
        var after = stat()
        guard fstat(descriptor, &after) == 0, result.count == Int(before.st_size),
              before.st_dev == after.st_dev, before.st_ino == after.st_ino,
              before.st_size == after.st_size, before.st_mtimespec.tv_sec == after.st_mtimespec.tv_sec,
              before.st_mtimespec.tv_nsec == after.st_mtimespec.tv_nsec else {
            throw NoticeError.invalid
        }
        return result
    }

    private enum NoticeError: Error {
        case unavailable
        case invalid
    }
}

private enum MacNoticeState: Sendable {
    case loading
    case ready(String)
    case unavailable
}

struct MacOpenSourceNoticesView: View {
    @Environment(\.dismiss) private var dismiss
    @State private var state = MacNoticeState.loading

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("Open Source Notices")
                .font(.title2.weight(.semibold))
                .accessibilityAddTraits(.isHeader)
            Group {
                switch state {
                case .loading:
                    ProgressView()
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                case let .ready(text):
                    ScrollView {
                        Text(text)
                            .font(.system(.caption, design: .monospaced))
                            .textSelection(.enabled)
                            .frame(maxWidth: .infinity, alignment: .leading)
                    }
                    .accessibilityIdentifier("settings.openSourceNotices.text")
                case .unavailable:
                    ContentUnavailableView(
                        "Notices Unavailable",
                        systemImage: "doc.text.magnifyingglass",
                        description: Text("The packaged notice files could not be verified.")
                    )
                }
            }
            .frame(minWidth: 560, minHeight: 420)
            HStack {
                Spacer()
                Button("Done") { dismiss() }
                    .keyboardShortcut(.defaultAction)
            }
        }
        .padding(24)
        .task {
            state = await Task.detached(priority: .userInitiated) {
                do { return .ready(try MacOpenSourceNotices.load()) }
                catch { return .unavailable }
            }.value
        }
    }
}
