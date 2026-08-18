import Foundation

extension AppModel {
    func connect(to account: GatewayAccount, retrying: Bool = false) {
        cancelReconnect()
        if !retrying {
            reconnectAttempt = 0
            automaticReconnectBlocked = false
        }
        let sameGateway = account.id == selectedAccountID
        let sessionID = sameGateway ? presentedChatSessionID : nil
        let generation = resetGatewayState(
            preservingDrafts: sameGateway,
            preservingSession: sessionID != nil
        )
        sessionToRestoreID = sessionID
        selectedAccountID = account.id
        store.select(account)
        connectionState = .connecting
        Task { [weak self] in
            guard let self, self.connectionGeneration == generation else { return }
            await self.client.disconnect()
            guard self.connectionGeneration == generation else { return }
            do {
                let token = try self.store.token(for: account)
                self.beginConnection(to: account.endpoint, generation: generation) { [weak self] in
                    guard let self, self.connectionGeneration == generation else { return }
                    try await self.requestSender(.authenticate(
                        token: token,
                        clientKind: .currentApplePlatform
                    ))
                }
            } catch {
                self.automaticReconnectBlocked = true
                self.connectionState = .failed(error.localizedDescription)
                self.showToast(error.localizedDescription, tone: .error)
                if let storeError = error as? GatewayStore.StoreError,
                   case .missingToken = storeError {
                    self.repairSelectedGateway()
                }
            }
        }
    }

    func beginConnection(
        to endpoint: GatewayEndpoint,
        generation: UUID,
        authenticate: @escaping @MainActor @Sendable () async throws -> Void
    ) {
        connectionState = .connecting

        Task { [weak self] in
            guard let self else { return }
            do {
                let stream = try await self.connectionOpener(endpoint)
                guard generation == self.connectionGeneration else { return }
                self.connectionState = .authenticating
                self.eventTask = Task { [weak self] in
                    do {
                        var handledFrames = 0
                        for try await frame in stream {
                            guard let self, generation == self.connectionGeneration else { return }
                            self.handle(frame)
                            handledFrames += 1
                            if handledFrames.isMultiple(of: 32) { await Task.yield() }
                        }
                        self?.connectionEnded(generation: generation, message: "The gateway closed the connection.")
                    } catch {
                        self?.connectionEnded(generation: generation, message: error.localizedDescription)
                    }
                }
                try await authenticate()
            } catch {
                self.connectionEnded(generation: generation, message: error.localizedDescription)
            }
        }
    }

    func transmit(
        _ request: GatewayRequest,
        onFailure: (@MainActor (String) -> Void)? = nil
    ) {
        let generation = connectionGeneration
        Task { [weak self] in
            guard let self, generation == self.connectionGeneration else { return }
            do {
                try await self.requestSender(request)
            } catch {
                guard generation == self.connectionGeneration else { return }
                let message = error.localizedDescription
                self.showToast(message, tone: .error)
                onFailure?(message)
            }
        }
    }

    func handle(_ envelope: GatewayEnvelope) {
        switch envelope {
        case .paired(_, let token):
            guard let account = pendingPairingAccount else { return }
            do {
                try store.save(account, token: token)
                accounts = store.loadAccounts()
                selectedAccountID = account.id
                pendingPairingAccount = nil
                pairingCode = ""
                showsPairing = false
                showToast("Gateway paired.", tone: .success)
            } catch {
                pairingError = error.localizedDescription
                showToast(error.localizedDescription, tone: .error)
            }
        case .authenticated:
            connectionState = .loading
        case .ready(let payload):
            applyGatewayReady(payload)
        case .sessionOpened(let requestID, let payload):
            guard requestID == sessionRequestID else { break }
            applySessionReady(payload, opened: true, replayRequestID: requestID)
        case .sessionReplayComplete(let requestID, let sessionID):
            guard requestID == replayRequestID, sessionID == selectedSessionID else { break }
            finishSessionReplay()
        case .sessionHistory(
            let requestID,
            let sessionID,
            let records,
            let nextBeforeSequence
        ):
            guard requestID == historyRequestID, sessionID == selectedSessionID else { break }
            flushStreamDeltas()
            mergeHistory(records)
            self.nextHistoryBeforeSequence = nextBeforeSequence
            if !records.isEmpty,
               case .visibleTurns(let count) = transcriptWindowAnchor {
                transcriptWindowAnchor = .visibleTurns(count + transcriptTurnsPerPage)
                _ = transcriptWindow
            }
            finishHistoryLoad()
        case .sessionChanged(let payload):
            guard payload.session.sessionId == selectedSessionID,
                  payload.config.revision >= (agentSnapshot?.revision ?? 0)
            else { break }
            applySessionReady(payload, opened: false)
        case .gatewayConfigured(let requestID, let payload):
            applyGatewayConfigurationResponse(requestID: requestID, payload: payload)
        case .accepted(let requestID):
            handleAccepted(requestID)
        case .rejected(let rejection):
            handleRejected(rejection)
        case .agentEvent(let sessionID, let record):
            guard sessionID == selectedSessionID else { break }
            let buffered = BufferedAgentEvent(record: record)
            applyAgentEvent(buffered)
            if replayRequestID == nil, shouldCacheTranscript(after: record.event) {
                cacheSelectedTranscript()
            }
        case .sessions(let requestID, let sessions):
            if requestID == sessionMutationRequestID {
                sessionMutationRequestID = nil
                pendingDeletedPresentedSessionID = nil
            }
            applySessions(sessions)
        case .clients:
            break
        case .providerCredentialSaved(let requestID, let provider):
            if let index = providerStatuses.firstIndex(where: { $0.provider == provider }) {
                providerStatuses[index].configured = true
            }
            if requestID == credentialRequestID {
                credentialRequestID = nil
                providerAPIKey = ""
                providerActionState = .credentialSaved(provider)
                showToast("\(provider) credential saved.", tone: .success)
            }
        case .pairingCode(let requestID, let code, let expiresAt):
            guard requestID == pairingCodeRequestID else { break }
            pairingCodeRequestID = nil
            setPairingCode(
                code,
                expiresAt: Date(timeIntervalSince1970: TimeInterval(expiresAt))
            )
        case .providerLoginStarted(let requestID, _, let provider, let url, let code):
            guard requestID == providerLoginRequestID else { break }
            providerActionState = .deviceCode(
                provider: provider,
                url: url,
                code: code
            )
        case .providerLoginFinished(let requestID, _, let provider):
            if requestID == providerLoginRequestID {
                providerLoginRequestID = nil
                providerActionState = .loginFinished(provider)
                showToast("Signed in to \(provider).", tone: .success)
            }
            if let index = providerStatuses.firstIndex(where: { $0.provider == provider }) {
                providerStatuses[index].configured = true
            }
        case .profile(_, let profile):
            self.profile = profile
        case .gitDiff(let requestID, let sessionID, let scope, let diff):
            guard sessionID == selectedSessionID else { break }
            if scope == .unstaged, requestID == gitDiffRequestID {
                gitDiffRequestID = nil
                isLoadingGitDiff = false
                gitDiff = diff
            } else if scope == .staged, requestID == stagedGitDiffRequestID {
                stagedGitDiffRequestID = nil
                isLoadingStagedGitDiff = false
                stagedGitDiff = diff
            } else if scope == .committed, requestID == committedGitDiffRequestID {
                committedGitDiffRequestID = nil
                isLoadingCommittedGitDiff = false
                committedGitDiff = diff
            }
        case .workspaceFiles(let requestID, let sessionID, let files, let truncated):
            guard requestID == workspaceFilesRequestID,
                  sessionID == selectedSessionID
            else { break }
            workspaceFilesRequestID = nil
            isLoadingWorkspaceFiles = false
            workspaceFiles = files
            workspaceFilesTruncated = truncated
        case .workspaceFileChunk(
            let requestID,
            let sessionID,
            let path,
            let offset,
            let data,
            let nextOffset
        ):
            handleWorkspaceFileChunk(
                requestID: requestID,
                sessionID: sessionID,
                path: path,
                offset: offset,
                data: data,
                nextOffset: nextOffset
            )
        case .sessionFileUploadReady(let requestID, let sessionID, let uploadID, let maxChunkBytes):
            handleSessionFileUploadReady(
                requestID: requestID,
                sessionID: sessionID,
                uploadID: uploadID,
                maxChunkBytes: maxChunkBytes
            )
        case .sessionFileUploadChunkAccepted(let requestID, let sessionID, let uploadID, let nextOffset):
            handleSessionFileUploadChunkAccepted(
                requestID: requestID,
                sessionID: sessionID,
                uploadID: uploadID,
                nextOffset: nextOffset
            )
        case .sessionFileUploadCompleted(let requestID, let sessionID, let file):
            handleSessionFileUploadCompleted(
                requestID: requestID,
                sessionID: sessionID,
                file: file
            )
        case .sessionUploads(let requestID, let sessionID, let uploads):
            guard requestID == sessionUploadsRequestID, sessionID == selectedSessionID else { break }
            sessionUploadsRequestID = nil
            isLoadingSessionUploads = false
            sessionUploads = uploads
        case .sessionFileChunk(
            let requestID,
            let sessionID,
            let fileID,
            let offset,
            let data,
            let nextOffset
        ):
            handleSessionFileChunk(
                requestID: requestID,
                sessionID: sessionID,
                fileID: fileID,
                offset: offset,
                data: data,
                nextOffset: nextOffset
            )
        case .directories(let requestID, let listing):
            guard requestID == directoryRequestID else { break }
            directoryRequestID = nil
            directoryListing = listing
            directoryError = nil
            isLoadingDirectories = false
        case .cronTasks(let requestID, let sessionID, let tasks):
            guard sessionID == selectedSessionID else { break }
            cronRequestIDs.remove(requestID)
            cronTasks = tasks
        case .cronHistory(let requestID, let sessionID, let runs):
            guard sessionID == selectedSessionID else { break }
            cronRequestIDs.remove(requestID)
            cronRuns = runs
        case .error(let failure):
            if pendingPairingAccount != nil { pairingError = failure.message }
            if failure.code == "unauthorized", pendingPairingAccount == nil {
                automaticReconnectBlocked = true
                cancelReconnect()
                repairSelectedGateway()
            }
            showToast(failure.message, tone: .error)
            if failure.fatal {
                automaticReconnectBlocked = true
                cancelReconnect()
                connectionGeneration = UUID()
                eventTask?.cancel()
                eventTask = nil
                restorePendingDrafts()
                extensionAction = nil
                extensionRequestID = nil
                connectionState = .failed(failure.message)
            }
        }
    }

    private func applyAgentEvent(_ buffered: BufferedAgentEvent) {
        guard latestSequence.map({ buffered.record.sequence > $0 }) ?? true else { return }
        let isLiveEvent = replayRequestID == nil
        observeReplayCompletion(buffered)
        latestSequence = buffered.record.sequence
        if isLiveEvent,
           buffered.record.event.msg["type"]?.stringValue == "context_compacted" {
            sessionCompactionCount += 1
        }
        transcriptRecords[buffered.record.sequence] = buffered.record
        reduce(
            record: buffered.record
        )
    }

    private func finishSessionReplay() {
        flushStreamDeltas()
        if let replaySnapshotSequence { latestSequence = replaySnapshotSequence }
        replayRequestID = nil
        replaySnapshotSequence = nil
        replayPresentedTranscript = nil
        connectionState = .ready
        completedComposerEditReplay = true
        reconcileChatTitleAfterReplay()
        reconcileComposerEditRecovery()
        requestSessionData()
        cacheSelectedTranscript()
    }

    /// A disconnected submission is ambiguous until replay proves whether it reached the
    /// checkpoint. Only then can the restored draft safely become title-eligible again.
    private func reconcileChatTitleAfterReplay() {
        guard let sessionID = selectedSessionID,
              let pending = pendingChatTitles[sessionID],
              !pending.submissionConfirmed
        else { return }
        let promptWasReplayed = replayCompletionSubmissionIDs.contains(
            pending.attempt.submissionID
        ) || replayUserMessages.contains {
            $0.text.trimmingCharacters(in: .whitespacesAndNewlines) == pending.attempt.prompt
        }
        if promptWasReplayed {
            confirmChatTitle(sessionID: sessionID)
        } else {
            cancelChatTitle(sessionID, rearm: true)
        }
    }

    private func shouldCacheTranscript(after event: AgentEventRecord) -> Bool {
        switch event.msg["type"]?.stringValue {
        case "task_complete", "turn_aborted": true
        default: false
        }
    }

    func cacheSelectedTranscript() {
        guard let accountID = selectedAccountID,
              let sessionID = selectedSessionID,
              let latestSequence,
              activeTurnID == nil,
              pendingApproval == nil,
              pendingWidgetEdit == nil
        else { return }
        let snapshot = CachedTranscript(
            sequence: latestSequence,
            nextBeforeSequence: nextHistoryBeforeSequence,
            transcript: transcript,
            currentUsage: currentUsage,
            lastUsage: lastUsage
        )
        enqueueTranscriptIO { [store] in
            await store.saveTranscript(
                snapshot,
                accountID: accountID,
                sessionID: sessionID
            )
        }
    }

    private func applyGatewayReady(_ payload: ReadyPayload) {
        cancelReconnect()
        reconnectAttempt = 0
        automaticReconnectBlocked = false
        applyGatewayCatalog(payload)
        if sessionRequestID == nil { connectionState = .ready }
        applySessions(payload.sessions)
        refreshProfile()
        guard sessionRequestID == nil else { return }
        if let sessionToRestoreID {
            guard presentedChatSessionID == sessionToRestoreID else {
                clearSelectedSession()
                return
            }
            if let session = sessions.first(where: { $0.sessionId == sessionToRestoreID }) {
                restoreSession(session.sessionId)
            } else {
                showToast("The previously selected chat is no longer available.", tone: .error)
                clearSelectedSession()
            }
        }
    }

    func applyGatewayConfigurationResponse(
        requestID: String,
        payload: ReadyPayload
    ) {
        let registeredProviderDraft = requestID == providerRegistrationRequestID
            ? defaultAgentDraft
            : nil
        let editedDefaultDraft = requestID == defaultConfigRequestID
            ? defaultAgentDraft
            : nil
        applyGatewayReady(payload)
        if requestID == providerRegistrationRequestID {
            providerRegistrationRequestID = nil
            defaultAgentApplyState = .idle
            if let registeredProviderDraft { defaultAgentDraft = registeredProviderDraft }
            applyAgentConfiguration(defaultAgentDraft, to: .defaultAgent)
        } else if requestID == defaultConfigRequestID {
            defaultConfigRequestID = nil
            if let editedDefaultDraft,
               let submittedDefaultAgentDraft,
               editedDefaultDraft != submittedDefaultAgentDraft {
                defaultAgentDraft = editedDefaultDraft
            }
            submittedDefaultAgentDraft = nil
            defaultAgentApplyState = .applied
            showToast("Default agent saved for new chats.", tone: .success)
        } else {
            completeExtensionAction(requestID: requestID)
        }
    }

    func applyGatewayCatalog(_ payload: ReadyPayload) {
        gatewayMachineName = payload.machineName
        rememberGatewayMachineName(payload.machineName)
        let previousDefault = defaultAgentSnapshot
        let pendingDefaultDraft: AgentComposition? = if defaultConfigRequestID != nil
            || providerRegistrationRequestID != nil {
            defaultAgentDraft
        } else {
            nil
        }
        providerStatuses = payload.providers
        modelChoices = payload.models
        modelProviders = payload.modelProviders
        middlewareFeatures = payload.middlewareFeatures
        extensions = payload.extensions
        gatewayContributions = payload.contributions
        defaultAgentSnapshot = payload.defaultConfig
        defaultAgentDraft = payload.defaultConfig.map { incomingSnapshot in
            pendingDefaultDraft ?? refreshedAgentDraft(
                currentDraft: defaultAgentDraft,
                currentSnapshot: previousDefault,
                incomingSnapshot: incomingSnapshot
            )
        }
        if providerDraft == nil, let provider = providerStatuses.first {
            selectProvider(provider.provider)
        }
    }

    private func rememberGatewayMachineName(_ machineName: String) {
        guard let account = selectedAccount,
              account.machineName != machineName,
              let index = accounts.firstIndex(where: { $0.id == account.id })
        else { return }
        accounts[index].machineName = machineName
        try? store.recordMachineName(machineName, for: account)
    }

    private func applySessionReady(
        _ payload: SessionReadyPayload,
        opened: Bool,
        replayRequestID: String? = nil
    ) {
        let createdByThisClient = opened && isChangingWorkspace
        let cursor = sessionOpenCursor
        let cached = opened && sessionOpeningID == payload.session.sessionId
            ? pendingCachedTranscript
            : nil
        let presented = opened && sessionOpeningID == payload.session.sessionId
            ? pendingPresentedTranscript
            : nil
        if selectedSessionID != payload.session.sessionId {
            restorePendingDrafts()
            changeComposerDraftOwner(to: selectedAccountID.map {
                ComposerDraftOwner(accountID: $0, sessionID: payload.session.sessionId)
            })
            resetSessionState()
        }
        if opened {
            latestSequence = cursor
            self.replayRequestID = replayRequestID
            replaySnapshotSequence = payload.latestSequence
            sessionOpenCursor = nil
            sessionOpeningID = nil
            pendingCachedTranscript = nil
            pendingPresentedTranscript = nil
            replayPresentedTranscript = presented ?? []
            transcriptRecordBase = cached?.transcript ?? []
            transcriptRecordBaseSequence = cursor
            transcriptRecords.removeAll(keepingCapacity: true)
            transcript = cached?.transcript ?? []
            if let cached {
                nextHistoryBeforeSequence = cached.nextBeforeSequence
            } else {
                nextHistoryBeforeSequence = payload.nextBeforeSequence
            }
            if let cached {
                currentUsage = cached.currentUsage
                lastUsage = cached.lastUsage
                updateContextTokens()
            }
        }
        sessionRequestID = nil
        workspace = payload.workspace
        gitStatus = payload.git
        workspaceError = nil
        isChangingWorkspace = false
        showsWorkspaceBrowser = false
        selectedSessionID = payload.session.sessionId
        if createdByThisClient {
            destination = .chats
            chatRoute = .session(payload.session.sessionId)
            prepareChatTitle(for: payload.session.sessionId)
        }
        if isChatVisible {
            unreadSessionIDs.remove(payload.session.sessionId)
        }
        selectedModelRoute = payload.session.model.route
        modelContextWindow = payload.session.model.modelContextWindow
        contributions = payload.contributions
        mountedWidgets = payload.contributions.flatMap { contribution in
            contribution.widgets.map {
                MountedWidget(capability: contribution.capability, widget: $0)
            }
        }
        for widget in payload.widgets {
            upsertWidget(MountedWidget(capability: widget.capability, widget: widget.item))
        }
        runStats = payload.runStats
        sessionCompactionCount = payload.compactionCount
        activeTurnID = payload.runStats.active?.turnId
        awaitsSteeringDelivery = false
        activeOperation = payload.contributions.compactMap(\.activeInput?.operation).first
        agentDraft = refreshedAgentDraft(
            currentDraft: agentDraft,
            currentSnapshot: agentSnapshot,
            incomingSnapshot: payload.config
        )
        agentSnapshot = payload.config
        if !opened { connectionState = .ready }
        if let accountID = selectedAccountID {
            prepareComposerEditRecovery(
                for: ComposerDraftOwner(
                    accountID: accountID,
                    sessionID: payload.session.sessionId
                )
            )
        }
        if chatAgentApplyState == .restarting {
            chatAgentApplyState = .applied
            showToast("Agent configuration applied.", tone: .success)
        }
        persistGeneratedChatTitles()
    }

    func applySessions(_ records: [SessionRecord]) {
        let visibleSessions = records.filter(\.catalogVisible)
        guard Set(visibleSessions.map(\.sessionId)).count == visibleSessions.count else {
            showToast("The gateway returned duplicate chat identifiers.", tone: .error)
            return
        }
        if sessions != visibleSessions {
            let previous = Dictionary(
                sessions.map { ($0.sessionId, $0.activity) },
                uniquingKeysWith: { _, latest in latest }
            )
            sessions = visibleSessions
            for session in sessions {
                applyActivityTransition(
                    from: previous[session.sessionId],
                    to: session.activity,
                    sessionID: session.sessionId
                )
            }
        }
        if let selected = sessions.first(where: { $0.sessionId == selectedSessionID }) {
            applyExecutionStats(selected.executionStats)
            if selected.activity.state == .idle { runStats.active = nil }
        }
        let visible = Set(sessions.map(\.sessionId))
        unreadSessionIDs.formIntersection(visible)
        reconcileChatTitles()
        guard let selectedSessionID,
              !sessions.contains(where: { $0.sessionId == selectedSessionID }),
              sessionRequestID == nil
        else { return }
        clearSelectedSession()
    }

    private func applyExecutionStats(_ stats: ExecutionStats) {
        runStats.runCount = stats.runCount
        runStats.failedRunCount = stats.failedRunCount
        runStats.abortedRunCount = stats.abortedRunCount
        runStats.modelCalls = stats.modelCalls
        runStats.toolCalls = stats.toolCalls
        runStats.failedToolCalls = stats.failedToolCalls
        runStats.elapsedMs = stats.elapsedMs
        runStats.usage = stats.usage
    }

    private func applyActivityTransition(
        from previous: SessionActivity?,
        to activity: SessionActivity,
        sessionID: String
    ) {
        guard let previous, previous != activity else { return }
        if activity.state == .awaitingApproval,
           previous.state != .awaitingApproval {
            showToast("\(sessionTitle(sessionID)) needs approval.", tone: .warning)
        }
        guard activity.state == .idle,
              let outcome = activity.lastOutcome,
              previous.state != .idle
                || previous.lastOutcome != outcome
                || previous.message != activity.message
        else { return }

        let isActiveChat = selectedSessionID == sessionID && isChatVisible
        if isActiveChat {
            unreadSessionIDs.remove(sessionID)
        } else {
            unreadSessionIDs.insert(sessionID)
        }

        switch outcome {
        case .completed:
            guard !isActiveChat else { return }
            showToast("\(sessionTitle(sessionID)) is ready.", tone: .success, sessionID: sessionID)
        case .aborted:
            guard !isActiveChat else { return }
            let detail = activity.message.map { ": \($0)" } ?? ""
            showToast("\(sessionTitle(sessionID)) stopped\(detail).", tone: .warning)
        case .failed:
            let detail = activity.message.map { ": \($0)" } ?? ""
            showToast("\(sessionTitle(sessionID)) failed\(detail).", tone: .error)
        }
    }

    private func requestSessionData() {
        guard selectedSessionID != nil else { return }
        refreshWorkspaceChanges()
        refreshSessionUploads()
        refreshCron()
    }

    func clearSelectedSession() {
        changeComposerDraftOwner(to: nil)
        latestSequence = nil
        sessionOpenCursor = nil
        sessionToRestoreID = nil
        selectedSessionID = nil
        chatRoute = nil
        resetSessionState()
        connectionState = .ready
    }

    private func handleAccepted(_ requestID: String) {
        if pendingDrafts[requestID] != nil { flushComposerDraft() }
        if requestID == approvalRequestID {
            pendingApproval = nil
            approvalRequestID = nil
        }
        if requestID == configRequestID {
            chatAgentApplyState = .restarting
            configRequestID = nil
        }
        if requestID == sessionMutationRequestID {
            if let sessionID = pendingDeletedSessionID {
                cancelChatTitle(sessionID)
                if let accountID = selectedAccountID {
                    let owner = ComposerDraftOwner(accountID: accountID, sessionID: sessionID)
                    invalidateComposerEditRecovery(for: owner)
                    enqueueComposerDraftSave("", owner: owner)
                    enqueueComposerEditRecoveryRemoval(owner: owner)
                    if composerDraftOwner == owner { discardComposerDraft() }
                }
            }
            pendingDeletedSessionID = nil
            pendingDeletedPresentedSessionID = nil
            transmit(.listSessions(requestID: requestID)) { [weak self] _ in
                if self?.sessionMutationRequestID == requestID {
                    self?.sessionMutationRequestID = nil
                }
            }
        }
        if requestID == gitBranchRequestID {
            gitBranchRequestID = nil
            showToast("Git branch changed.", tone: .success)
            refreshWorkspaceChanges()
        }
        if cronRequestIDs.remove(requestID) != nil {
            cronTaskDraft = ""
            refreshCron()
        }
    }

    private func handleRejected(_ rejection: GatewayRejection) {
        let deletedPresentedSessionID = rejection.requestId == sessionMutationRequestID
            ? pendingDeletedPresentedSessionID
            : nil
        if rejection.requestId == historyRequestID {
            finishHistoryLoad()
        }
        if rejection.requestId == previewPageRequestID {
            previewPageRequestID = nil
            isLoadingPreviewPage = false
        }
        if rejection.requestId == sessionMutationRequestID {
            pendingDeletedSessionID = nil
            pendingDeletedPresentedSessionID = nil
            if let sessionID = pendingChatTitles.first(where: {
                $0.value.renameRequestID == rejection.requestId
            })?.key {
                cancelChatTitle(sessionID)
            }
        }
        cancelChatTitle(submissionID: rejection.requestId, rearm: true)
        if rejection.requestId == sessionRequestID,
           rejection.code == "replay_unavailable",
           let sessionID = sessionOpeningID,
           sessionOpenCursor != nil {
            if let accountID = selectedAccountID {
                enqueueTranscriptIO { [store] in
                    await store.removeTranscript(accountID: accountID, sessionID: sessionID)
                }
            }
            sessionRequestID = nil
            sessionOpenCursor = nil
            pendingCachedTranscript = nil
            pendingPresentedTranscript = nil
            if sessionID == selectedSessionID { resetSessionState() }
            requestSessionOpen(sessionID, lastSequence: nil)
            return
        }
        failSessionFileUploadRequest(rejection.requestId, message: rejection.message, showsToast: false)
        if rejection.requestId == sessionUploadsRequestID {
            sessionUploadsRequestID = nil
            isLoadingSessionUploads = false
        }
        if rejection.requestId == sessionFileDownload?.requestID {
            sessionFileDownload = nil
            isLoadingFilePresentation = false
        }
        if rejection.requestId == workspaceFilePreviewDownload?.requestID {
            workspaceFilePreviewDownload = nil
            isLoadingFilePresentation = false
        }
        if pendingDrafts[rejection.requestId] != nil {
            restoreDraft(id: rejection.requestId)
        }
        rejectComposerEdit(requestID: rejection.requestId)
        if rejection.requestId == configRequestID
            || rejection.requestId == defaultConfigRequestID {
            let state: ApplyState = switch rejection.code {
            case "revision_conflict": .conflict(rejection.message)
            case "agent_busy": .busy(rejection.message)
            case "invalid_config": .invalid(rejection.message)
            default: .failed(rejection.message)
            }
            if rejection.requestId == configRequestID {
                chatAgentApplyState = state
                configRequestID = nil
            }
            if rejection.requestId == defaultConfigRequestID {
                defaultAgentApplyState = state
                defaultConfigRequestID = nil
                submittedDefaultAgentDraft = nil
            }
        }
        if rejection.requestId == approvalRequestID {
            approvalRequestID = nil
        }
        if rejection.requestId == sessionRequestID {
            sessionRequestID = nil
            sessionOpeningID = nil
            sessionOpenCursor = nil
            pendingCachedTranscript = nil
            pendingPresentedTranscript = nil
            connectionState = .ready
            if isChangingWorkspace { workspaceError = rejection.message }
            isChangingWorkspace = false
        }
        if rejection.requestId == sessionMutationRequestID {
            sessionMutationRequestID = nil
            restoreDeletedPresentedSession(deletedPresentedSessionID)
        }
        if rejection.requestId == directoryRequestID {
            directoryError = rejection.message
            directoryRequestID = nil
            isLoadingDirectories = false
        }
        if rejection.requestId == gitDiffRequestID {
            gitDiffRequestID = nil
            isLoadingGitDiff = false
        }
        if rejection.requestId == stagedGitDiffRequestID {
            stagedGitDiffRequestID = nil
            isLoadingStagedGitDiff = false
        }
        if rejection.requestId == committedGitDiffRequestID {
            committedGitDiffRequestID = nil
            isLoadingCommittedGitDiff = false
        }
        if rejection.requestId == workspaceFilesRequestID {
            workspaceFilesRequestID = nil
            isLoadingWorkspaceFiles = false
        }
        if rejection.requestId == gitBranchRequestID {
            gitBranchRequestID = nil
        }
        if rejection.requestId == credentialRequestID {
            providerActionState = .failed(rejection.message)
            credentialRequestID = nil
        }
        if rejection.requestId == providerLoginRequestID {
            providerActionState = .failed(rejection.message)
            providerLoginRequestID = nil
        }
        if rejection.requestId == providerRegistrationRequestID {
            defaultAgentApplyState = .failed(rejection.message)
            providerRegistrationRequestID = nil
        }
        rejectExtensionAction(requestID: rejection.requestId)
        if rejection.requestId == pairingCodeRequestID {
            pairingCodeRequestID = nil
        }
        if cronRequestIDs.remove(rejection.requestId) != nil {
            cronError = rejection.message
        }
        showToast(
            rejection.message,
            tone: rejection.code == "revision_conflict" || rejection.code == "agent_busy"
                ? .warning
                : .error
        )
        if rejection.fatal {
            automaticReconnectBlocked = true
            cancelReconnect()
            connectionGeneration = UUID()
            eventTask?.cancel()
            eventTask = nil
            restorePendingDrafts()
            extensionAction = nil
            extensionRequestID = nil
            connectionState = .failed(rejection.message)
        }
    }

}
