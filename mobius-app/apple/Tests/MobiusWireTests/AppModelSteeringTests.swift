import Foundation
import XCTest

@MainActor
extension AppModelTests {
    func testSteeringDraftSettlesOnSuccessAndRestoresOnWarning() async throws {
        let recorder = GatewayRequestRecorder()
        let model = try model(requestSender: { request in
            await recorder.record(request)
        })
        model.connectionState = .ready
        model.selectedSessionID = "chat-1"
        model.activeTurnID = "turn-1"
        model.activeOperation = "steer"
        model.composer = "Use the smaller patch"

        var requestCount = await recorder.requestCount()
        model.sendMessage()
        let firstRequest = await recorder.firstRequest(after: requestCount) { request in
            if case .submit = request { return true }
            return false
        }
        let first = try XCTUnwrap(firstRequest.flatMap { request -> Submission? in
            guard case .submit(_, let submission) = request else { return nil }
            return submission
        })
        model.reduce(
            event: AgentEventRecord(submissionId: first.id, msg: .object([
                "type": .string("frontend"),
                "frontendType": .string("widget"),
                "capability": .string("steering"),
                "item": .object([
                    "id": .string(first.id),
                    "slot": .string("transcript_tail"),
                    "text": .string("Use the smaller patch"),
                    "tone": .string("neutral"),
                    "symbol": .null,
                    "iconOnly": .bool(false),
                    "progress": .null,
                    "content": .null,
                    "action": .object([
                        "type": .string("capability_command"),
                        "capability": .string("steering"),
                        "command": .string("edit"),
                        "arguments": .string(first.id),
                        "input": .string("Use the smaller patch"),
                        "target": .null
                    ])
                ])
            ])),
            blocks: [],
            preview: nil
        )
        model.handle(.rejected(GatewayRejection(
            requestId: "unrelated",
            code: "connection_failed",
            message: "Disconnected",
            fatal: true
        )))

        XCTAssertEqual(model.composer, "")
        XCTAssertEqual(model.transcriptTailWidgets.first?.widget.text, "Use the smaller patch")
        XCTAssertEqual(
            model.transcriptTailWidgets.first?.widget.action?.capabilityInput,
            "Use the smaller patch"
        )

        model.connectionState = .ready
        model.composer = "Retry this steering"
        requestCount = await recorder.requestCount()
        model.sendMessage()
        let secondRequest = await recorder.firstRequest(after: requestCount) { request in
            if case .submit = request { return true }
            return false
        }
        let second = try XCTUnwrap(secondRequest.flatMap { request -> Submission? in
            guard case .submit(_, let submission) = request else { return nil }
            return submission
        })
        model.reduce(
            event: AgentEventRecord(submissionId: second.id, msg: .object([
                "type": .string("warning"),
                "message": .string("Steering queue is full")
            ])),
            blocks: [],
            preview: nil
        )

        XCTAssertEqual(model.composer, "Retry this steering")
    }

    func testQueuedSteeringKeepsOneBubblePerMessageAndRemovesOnlyTheTarget() throws {
        let model = try model()
        for (id, text) in [("steer-1", "First"), ("steer-2", "Second")] {
            model.reduce(
                event: AgentEventRecord(submissionId: id, msg: .object([
                    "type": .string("frontend"),
                    "frontendType": .string("widget"),
                    "capability": .string("steering"),
                    "item": .object([
                        "id": .string(id),
                        "slot": .string("transcript_tail"),
                        "text": .string(text),
                        "tone": .string("neutral"),
                        "symbol": .null,
                        "iconOnly": .bool(false),
                        "progress": .null,
                        "content": .null,
                        "action": .null
                    ])
                ])),
                blocks: [],
                preview: nil
            )
        }

        XCTAssertEqual(model.transcriptTailWidgets.map(\.widget.text), ["First", "Second"])

        model.reduce(
            event: AgentEventRecord(submissionId: "input-1", msg: .object([
                "type": .string("frontend"),
                "frontendType": .string("remove_widget"),
                "capability": .string("steering"),
                "id": .string("steer-1")
            ])),
            blocks: [],
            preview: nil
        )

        XCTAssertEqual(model.transcriptTailWidgets.map(\.widget.text), ["Second"])

        model.reduce(
            event: AgentEventRecord(submissionId: "input-1", msg: .object([
                "type": .string("frontend"),
                "frontendType": .string("remove_widget"),
                "capability": .string("steering"),
                "id": .string("steer-2")
            ])),
            blocks: [],
            preview: nil
        )

        XCTAssertTrue(model.transcriptTailWidgets.isEmpty)
    }

    func testSteeringFeedbackWaitsUntilTheQueuedMessageReachesTheAgent() throws {
        let model = try model()
        model.contributions = [FrontendContribution(
            capability: "steering",
            acceptsFileAttachments: false,
            count: nil,
            commands: [],
            widgets: [],
            references: [],
            activeInput: FrontendActiveInput(operation: "steer")
        )]
        model.reduce(
            event: AgentEventRecord(submissionId: "input-1", msg: .object([
                "type": .string("task_started"),
                "turnId": .string("turn-1")
            ])),
            blocks: [],
            preview: nil
        )
        model.reduce(
            event: AgentEventRecord(submissionId: "input-1", msg: .object([
                "type": .string("user_message"),
                "message": .string("Start"),
                "attachments": .array([]),
                "messageTarget": .object([
                    "checkpointSequence": .number(1),
                    "batchItemCount": .number(1)
                ])
            ])),
            blocks: [],
            preview: nil
        )
        model.reduce(
            event: AgentEventRecord(submissionId: "input-1", msg: .object([
                "type": .string("frontend"),
                "frontendType": .string("remove_widget"),
                "capability": .string("steering"),
                "id": .string("steering-1")
            ])),
            blocks: [],
            preview: nil
        )

        XCTAssertEqual(model.steeringDeliveryRevision, 0)

        model.reduce(
            event: AgentEventRecord(submissionId: "input-1", msg: .object([
                "type": .string("user_message"),
                "message": .string("Use the smaller patch"),
                "attachments": .array([]),
                "messageTarget": .object([
                    "checkpointSequence": .number(2),
                    "batchItemCount": .number(1)
                ])
            ])),
            blocks: [],
            preview: nil
        )

        XCTAssertEqual(model.steeringDeliveryRevision, 1)
    }

}
