import XCTest

final class UnifiedDiffTests: XCTestCase {
    func testParsesFilesHunksCountsAndLineNumbers() throws {
        let document = UnifiedDiffDocument(
            """
            diff --git a/Sources/App.swift b/Sources/App.swift
            index 1111111..2222222 100644
            --- a/Sources/App.swift
            +++ b/Sources/App.swift
            @@ -10,3 +10,4 @@ func render() {
             keep
            -old
            +new
            +++ enabled
             tail
            \\ No newline at end of file
            diff --git a/README.md b/README.md
            new file mode 100644
            --- /dev/null
            +++ b/README.md
            @@ -0,0 +1,2 @@
            +# Horus
            +Native client
            """
        )

        XCTAssertEqual(document.files.count, 2)
        XCTAssertEqual(document.added, 4)
        XCTAssertEqual(document.removed, 1)

        let swift = try XCTUnwrap(document.files.first)
        XCTAssertEqual(swift.path, "Sources/App.swift")
        XCTAssertEqual(swift.added, 2)
        XCTAssertEqual(swift.removed, 1)

        guard case let .hunk(hunk) = swift.rows[0].kind else {
            return XCTFail("Expected a hunk header")
        }
        XCTAssertEqual(hunk.title, "Lines 10–13")
        XCTAssertEqual(hunk.added, 2)
        XCTAssertEqual(hunk.removed, 1)
        XCTAssertEqual(swift.rows[1].oldNumber, 10)
        XCTAssertEqual(swift.rows[1].newNumber, 10)
        XCTAssertEqual(swift.rows[2].oldNumber, 11)
        XCTAssertNil(swift.rows[2].newNumber)
        XCTAssertEqual(swift.rows[3].newNumber, 11)
        XCTAssertEqual(swift.rows[4].text, "++ enabled")
        XCTAssertEqual(swift.rows[4].newNumber, 12)
        XCTAssertNil(swift.rows[6].oldNumber)
        XCTAssertNil(swift.rows[6].newNumber)

        let readme = document.files[1]
        XCTAssertEqual(readme.path, "README.md")
        XCTAssertEqual(readme.added, 2)
        XCTAssertEqual(readme.rows[1].newNumber, 1)
        XCTAssertEqual(readme.rows[2].newNumber, 2)

        let deletionHeavyHunk = UnifiedDiffHunk(
            oldRange: UnifiedDiffRange(start: 128, count: 11),
            newRange: UnifiedDiffRange(start: 128, count: 3),
            added: 1,
            removed: 9
        )
        XCTAssertEqual(deletionHeavyHunk.title, "Lines 128–138")
    }

    func testKeepsMetadataOnlyFilesAndTruncation() throws {
        let document = UnifiedDiffDocument(
            """
            diff --git a/script.sh b/script.sh
            old mode 100644
            new mode 100755
            diff --git a/large.txt b/large.txt
            --- a/large.txt
            +++ b/large.txt
            @@ -1 +1 @@
            -before
            +after
            [diff truncated]
            """
        )

        XCTAssertTrue(document.isTruncated)
        XCTAssertEqual(document.files.count, 2)
        XCTAssertEqual(document.files[0].path, "script.sh")
        XCTAssertEqual(document.files[0].rows.map(\.text), ["old mode 100644", "new mode 100755"])
        XCTAssertEqual(document.files[1].rows.last?.text, "after")
    }

    func testParsesStandaloneToolPatchWithoutGitHeader() throws {
        let document = UnifiedDiffDocument(
            """
            --- note.txt
            +++ note.txt
            @@ -1,3 +1,3 @@
             first
            -old
            +new
             last
            """
        )

        let file = try XCTUnwrap(document.files.first)
        XCTAssertEqual(document.files.count, 1)
        XCTAssertEqual(file.path, "note.txt")
        XCTAssertEqual(file.added, 1)
        XCTAssertEqual(file.removed, 1)
        XCTAssertEqual(file.rows.map(\.text), ["@@ -1,3 +1,3 @@", "first", "old", "new", "last"])
    }

    func testBoundsOneMinifiedLineBeforeRendering() throws {
        let source = """
        diff --git a/data.json b/data.json
        --- a/data.json
        +++ b/data.json
        @@ -0,0 +1 @@
        +\(String(repeating: "x", count: 20_000))
        """
        let document = UnifiedDiffDocument(source)
        let line = try XCTUnwrap(document.files.first?.rows.last)

        XCTAssertLessThan(line.text.count, 4_200)
        XCTAssertTrue(line.text.hasSuffix("[line truncated]"))
    }
}
