// Application states for the WebTransport Upload Client
export const AppState = {
    INIT: 'init',                           // Initial state, no connection
    CONNECTING: 'connecting',               // Attempting to establish connection(s)
    CONNECTED: 'connected',                 // Connected but not uploading
    WARMUP: 'warmup',                        // Preparing upload
    RUNNING: 'running',                      // Actively uploading
    STOPPING: 'stopping',                    // Stop requested, finishing remaining data
    STOPPED_WITH_RESULTS: 'stopped_with_results', // Upload stopped, results available
    RECOVERING: 'recovering',                // Connection lost, attempting recovery
    CRASHED: 'crashed',                      // Unrecoverable error state
    DISCONNECTING: 'disconnecting'           // Gracefully disconnecting
};

// Valid state transitions
export const StateTransitions = {
    [AppState.INIT]: [AppState.CONNECTING],
    [AppState.CONNECTING]: [AppState.CONNECTED, AppState.CRASHED, AppState.INIT],
    [AppState.CONNECTED]: [AppState.WARMUP, AppState.DISCONNECTING, AppState.RECOVERING],
    [AppState.WARMUP]: [AppState.RUNNING, AppState.STOPPING, AppState.RECOVERING, AppState.CRASHED, AppState.DISCONNECTING],
    [AppState.RUNNING]: [AppState.STOPPING, AppState.STOPPED_WITH_RESULTS, AppState.RECOVERING, AppState.CRASHED, AppState.DISCONNECTING],
    [AppState.STOPPING]: [AppState.STOPPED_WITH_RESULTS, AppState.RECOVERING, AppState.CRASHED, AppState.DISCONNECTING],
    [AppState.STOPPED_WITH_RESULTS]: [AppState.WARMUP, AppState.DISCONNECTING, AppState.RECOVERING],
    [AppState.RECOVERING]: [AppState.CONNECTED, AppState.CRASHED, AppState.RUNNING, AppState.DISCONNECTING],
    [AppState.CRASHED]: [AppState.INIT],
    [AppState.DISCONNECTING]: [AppState.INIT]
};

export class StateManager {
    constructor(initialState = AppState.INIT) {
        this.currentState = initialState;
        this.previousState = null;
        this.stateHistory = [initialState];
        this.listeners = [];
        this.locked = false;
    }

    async transition(newState, reason = '', data = null) {
        if (this.locked) {
            console.warn('State transition blocked: state manager is locked');
            return false;
        }

        const allowedTransitions = StateTransitions[this.currentState] || [];
        if (!allowedTransitions.includes(newState)) {
            console.error(`Invalid state transition: ${this.currentState} -> ${newState}`);
            return false;
        }

        const oldState = this.currentState;
        this.previousState = oldState;
        this.currentState = newState;
        this.stateHistory.push(newState);

        // Keep history manageable
        if (this.stateHistory.length > 50) {
            this.stateHistory = this.stateHistory.slice(-25);
        }

        const stateChange = {
            from: oldState,
            to: newState,
            reason,
            data,
            timestamp: performance.now()
        };

        this.notifyListeners(stateChange);
        return true;
    }

    getState() {
        return this.currentState;
    }

    isState(state) {
        return this.currentState === state;
    }

    isAnyState(...states) {
        return states.includes(this.currentState);
    }

    addListener(callback) {
        this.listeners.push(callback);
        return () => {
            const index = this.listeners.indexOf(callback);
            if (index > -1) {
                this.listeners.splice(index, 1);
            }
        };
    }

    notifyListeners(stateChange) {
        for (const listener of this.listeners) {
            try {
                listener(stateChange);
            } catch (error) {
                console.error('Error in state listener:', error);
            }
        }
    }

    lock() {
        this.locked = true;
    }

    unlock() {
        this.locked = false;
    }

    getHistory() {
        return [...this.stateHistory];
    }

    // State permission helpers
    canStartUpload() {
        return this.isAnyState(AppState.CONNECTED, AppState.STOPPED_WITH_RESULTS);
    }

    canStopUpload() {
        return this.isAnyState(AppState.WARMUP, AppState.RUNNING);
    }

    canConnect() {
        return this.isState(AppState.INIT);
    }

    canDisconnect() {
        return !this.isAnyState(AppState.INIT, AppState.DISCONNECTING, AppState.CRASHED);
    }

    isCollectingData() {
        return this.isAnyState(AppState.WARMUP, AppState.RUNNING, AppState.STOPPING);
    }

    isConnected() {
        return !this.isAnyState(AppState.INIT, AppState.CONNECTING, AppState.CRASHED, AppState.DISCONNECTING);
    }
}

// Helper to create a state change message for workers
export function createStateMessage(state, reason = '', data = null) {
    return {
        type: 'state_change',
        state,
        reason,
        data,
        timestamp: performance.now()
    };
}