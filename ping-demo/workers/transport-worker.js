import { AppState } from '../state.js';

class TransportWorker {
    constructor() {
        this.transport = null;
        this.reader = null;
        this.currentState = AppState.INIT;
        this.wasRunningBeforeRecovery = false;
        
        this.benchmarkConfig = {
            interval: 10,
            url: ''
        };
        
        this.stats = {
            sent: 0,
            received: 0,
            pendingPings: new Map()
        };
        
        this.pingInterval = null;
        this.gracePeriodTimeout = null;
        this.recoveryTimeout = null;
        
        this.recoveryConfig = {
            maxAttempts: 10,
            maxRecoveryTime: 5000, // 5 seconds
            initialDelay: 250, // Start with 250ms delay to give server time
            maxDelay: 2000,
            currentAttempt: 0,
            startTime: 0
        };
        
        this.isManualDisconnect = false;
        this.isRecovering = false; // Prevent multiple recovery processes
        
        this.setupMessageHandler();
    }

    setupMessageHandler() {
        self.onmessage = (e) => {
            const { type, data, state, reason } = e.data;
            
            if (type === 'state_change') {
                this.handleStateChange(state, reason, data);
            }
        };
    }

    handleStateChange(newState, reason, data) {
        const oldState = this.currentState;
        this.currentState = newState;
        
        switch(newState) {
            case AppState.CONNECTING:
                if (data && data.url) {
                    this.handleConnectingState(data.url);
                } else {
                    console.error('[TRANSPORT] CONNECTING state but no URL provided:', data);
                }
                break;
            case AppState.WARMUP:
                if (data && data.interval) {
                    this.handleWarmupState(data.interval);
                }
                break;
            case AppState.STOPPING:
                this.handleStoppingState();
                break;
            case AppState.CRASHED:
                this.handleCrashState();
                break;
            case AppState.DISCONNECTING:
                this.handleDisconnectingState();
                break;
        }
    }

    async handleConnectingState(url) {
        this.benchmarkConfig.url = url;
        this.isManualDisconnect = false;
        
        try {
            this.transport = new WebTransport(url, {
                allowPooling: false,
                congestionControl: 'low-latency',
                requireUnreliable: true
            });
            
            await this.transport.ready;
            
            this.transport.closed.then(() => {
                this.handleConnectionClosed('Transport closed gracefully');
            }).catch((error) => {
                this.handleConnectionClosed(`Transport error: ${error.message}`);
            });
            
            this.startReading();
            this.resetRecoveryState();
            
            this.postMessage('state_change', { 
                state: AppState.CONNECTED, 
                reason: 'WebTransport connection established',
                data: { url }
            });
            
        } catch (error) {
            console.error('[TRANSPORT] Connection failed:', error);
            
            this.postMessage('state_change', { 
                state: AppState.CRASHED, 
                reason: `Connection failed: ${error.message}`,
                data: { error: error.message }
            });
        }
    }

    handleWarmupState(interval) {
        this.benchmarkConfig.interval = interval;
        
        this.stats.sent = 0;
        this.stats.received = 0;
        this.stats.pendingPings.clear();
        
        this.pingInterval = setInterval(() => this.sendPing(), interval);
        
        this.postMessage('state_change', {
            state: AppState.RUNNING,
            reason: 'Benchmark started',
            data: { interval }
        });
    }

    handleStoppingState() {
        if (this.pingInterval) {
            clearInterval(this.pingInterval);
            this.pingInterval = null;
        }
        
        // After grace period, transition to STOPPED_WITH_RESULTS
        this.gracePeriodTimeout = setTimeout(() => {
            this.postMessage('state_change', {
                state: AppState.STOPPED_WITH_RESULTS,
                reason: 'Benchmark completed'
            });
        }, 500);
    }

    handleCrashState() {
        this.cleanupAll();
    }

    handleDisconnectingState() {
        this.isManualDisconnect = true;
        this.cleanupAll();
        
        // Just update internal state without posting a state change message
        this.currentState = AppState.INIT;
        // Let the main app handle the transition to INIT state
    }

    handleConnectionClosed(reason) {
        if (this.isManualDisconnect || this.isRecovering) {
            return;
        }
        
        if (this.shouldAttemptRecovery()) {
            this.startRecovery(reason);
        } else {
            this.postMessage('state_change', { 
                state: AppState.CRASHED, 
                reason: `Connection lost: ${reason}`,
                data: { canRecover: false }
            });
        }
    }

    shouldAttemptRecovery() {
        return !this.isManualDisconnect && 
               !this.isRecovering &&
               this.currentState !== AppState.INIT &&
               this.currentState !== AppState.CRASHED &&
               this.currentState !== AppState.DISCONNECTING;
    }

    startRecovery(reason) {
        this.isRecovering = true;
        this.wasRunningBeforeRecovery = this.currentState === AppState.RUNNING;
        this.currentState = AppState.RECOVERING;
        
        if (this.pingInterval) {
            clearInterval(this.pingInterval);
            this.pingInterval = null;
        }
        
        this.recoveryConfig.currentAttempt = 0;
        this.recoveryConfig.startTime = performance.now();
        
        const stateChangeData = { 
            wasRunning: this.wasRunningBeforeRecovery,
            willResume: this.wasRunningBeforeRecovery 
        };
        
        this.postMessage('state_change', { 
            state: AppState.RECOVERING, 
            reason: `Connection lost: ${reason}`,
            data: stateChangeData
        });
        
        this.scheduleRecoveryAttempt();
    }

    scheduleRecoveryAttempt() {
        if (!this.isRecovering) {
            return;
        }
        
        const elapsed = performance.now() - this.recoveryConfig.startTime;
        
        if (elapsed >= this.recoveryConfig.maxRecoveryTime || 
            this.recoveryConfig.currentAttempt >= this.recoveryConfig.maxAttempts) {
            this.isRecovering = false;
            this.postMessage('state_change', { 
                state: AppState.CRASHED, 
                reason: 'Connection recovery failed: Maximum attempts or time exceeded'
            });
            return;
        }
        
        this.recoveryConfig.currentAttempt++;
        
        // Exponential backoff with jitter
        const baseDelay = Math.min(
            this.recoveryConfig.initialDelay * Math.pow(2, this.recoveryConfig.currentAttempt - 1),
            this.recoveryConfig.maxDelay
        );
        const jitter = Math.random() * 0.1 * baseDelay;
        const delay = baseDelay + jitter;
        
        this.recoveryTimeout = setTimeout(() => this.attemptRecovery(), delay);
    }

    async attemptRecovery() {
        if (this.isManualDisconnect || !this.isRecovering) {
            return;
        }
        
        try {
            this.postMessage('recovery_attempt', {
                data: {
                    attempt: this.recoveryConfig.currentAttempt,
                    message: `Attempting reconnection...`,
                    totalAttempts: this.recoveryConfig.maxAttempts
                }
            });
            
            // Cleanup old connection
            if (this.reader) {
                this.reader.cancel();
                this.reader = null;
            }
            
            if (this.transport) {
                try {
                    await this.transport.close();
                } catch (e) {
                    // Ignore errors during cleanup
                }
                this.transport = null;
            }
            
            // Attempt new connection
            this.transport = new WebTransport(this.benchmarkConfig.url, {
                allowPooling: false,
                congestionControl: 'low-latency',
                requireUnreliable: true
            });
            
            await this.transport.ready;
            
            this.transport.closed.then(() => {
                this.handleConnectionClosed('Connection closed by server');
            }).catch((error) => {
                this.handleConnectionClosed(`Connection error: ${error.message}`);
            });
            
            this.startReading();
            this.isRecovering = false;
            
            // Successfully recovered
            if (this.wasRunningBeforeRecovery) {
                // Resume the benchmark if it was running before
                this.resumeBenchmark();
                this.postMessage('state_change', {
                    state: AppState.RUNNING,
                    reason: 'Connection recovered, test resumed',
                    data: { recovered: true }
                });
            } else {
                this.postMessage('state_change', {
                    state: AppState.CONNECTED,
                    reason: 'Connection recovered',
                    data: { recovered: true }
                });
            }
            
            this.resetRecoveryState();
            
        } catch (error) {
            // Schedule another attempt
            this.scheduleRecoveryAttempt();
        }
    }

    resetRecoveryState() {
        this.recoveryConfig.currentAttempt = 0;
        this.recoveryConfig.startTime = 0;
        this.wasRunningBeforeRecovery = false;
        this.isRecovering = false;
        
        if (this.recoveryTimeout) {
            clearTimeout(this.recoveryTimeout);
            this.recoveryTimeout = null;
        }
    }

    resumeBenchmark() {
        if (this.pingInterval) {
            clearInterval(this.pingInterval);
            this.pingInterval = null;
        }
        
        // Restart the ping interval
        this.pingInterval = setInterval(() => this.sendPing(), this.benchmarkConfig.interval);
        
        // Send an initial ping to get things going immediately
        setTimeout(() => this.sendPing(), 0);
    }

    async startReading() {
        if (!this.transport) return;
        
        try {
            this.reader = this.transport.datagrams.readable.getReader();
            
            while (true) {
                const { value, done } = await this.reader.read();
                
                if (done) break;
                
                const receiveTime = performance.now();
                const message = new TextDecoder().decode(value);
                
                // Only process PONG messages
                if (message.startsWith('PONG,')) {
                    this.processPong(message, receiveTime);
                }
            }
        } catch (error) {
            if (!this.isManualDisconnect) {
                console.error('[TRANSPORT] Reading error:', error);
                this.handleConnectionClosed(`Reading error: ${error.message}`);
            }
        }
    }

    processPong(message, receiveTime) {
        // Expected format: "PONG,<uuid>;TIME,<timestamp>"
        const semicolon = message.indexOf(';');
        if (semicolon === -1 || !message.startsWith('PONG,')) return;
        
        const uuid = message.substring(5, semicolon); // Skip "PONG,"
        
        if (this.stats.pendingPings.has(uuid)) {
            const sendTime = this.stats.pendingPings.get(uuid);
            
            // Count pongs during RUNNING, STOPPING or RECOVERING
            if (this.shouldCountPong()) {
                this.stats.received++;
                this.stats.pendingPings.delete(uuid);
                
                this.postMessage('pong_received', {
                    data: {
                        uuid,
                        sendTime: sendTime + performance.timeOrigin,
                        receiveTime: receiveTime + performance.timeOrigin,
                        sent: this.stats.sent,
                        received: this.stats.received
                    }
                });
            } else {
                // Remove from pending but don't count
                this.stats.pendingPings.delete(uuid);
            }
        }
    }

    shouldCountPong() {
        // Count pongs during RUNNING, STOPPING (which includes grace period), and RECOVERING
        return this.currentState === AppState.RUNNING ||
               this.currentState === AppState.STOPPING ||
               this.currentState === AppState.RECOVERING;
    }

    async sendPing() {
        try {
            const uuid = this.generateUUID();
            const sendTime = performance.now();
            const message = `PING,${uuid}`;
            
            this.stats.pendingPings.set(uuid, sendTime);
            this.stats.sent++;
            
            const writer = this.transport.datagrams.writable.getWriter();
            await writer.write(new TextEncoder().encode(message));
            writer.releaseLock();
            
            this.postMessage('ping_sent', {
                data: {
                    uuid,
                    sendTime: sendTime + performance.timeOrigin,
                    sent: this.stats.sent
                }
            });
            
        } catch (error) {
            console.error('[TRANSPORT] Send error:', error);
            this.postMessage('error', { data: { error: `Send error: ${error.message}` } });
        }
    }

    cleanupAll() {
        // Clear timers
        if (this.pingInterval) {
            clearInterval(this.pingInterval);
            this.pingInterval = null;
        }
        
        if (this.gracePeriodTimeout) {
            clearTimeout(this.gracePeriodTimeout);
            this.gracePeriodTimeout = null;
        }
        
        if (this.recoveryTimeout) {
            clearTimeout(this.recoveryTimeout);
            this.recoveryTimeout = null;
        }
        
        this.isRecovering = false;
        
        // Cancel reader
        if (this.reader) {
            this.reader.cancel().catch(() => {}).finally(() => {});
            this.reader = null;
        }
        
        // Close transport
        if (this.transport) {
            try {
                this.transport.close();
            } catch (e) {
                // Ignore expected errors during cleanup
            }
            this.transport = null;
        }
        
        this.resetRecoveryState();
    }

    generateUUID() {
        return 'xxxx-xxxx-4xxx'.replace(/[x]/g, () => 
            (Math.random() * 16 | 0).toString(16)
        );
    }

    postMessage(type, payload = {}) {
        self.postMessage({ type, ...payload });
    }
}

// Initialize worker
const worker = new TransportWorker();
