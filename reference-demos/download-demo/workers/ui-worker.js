import { AppState } from '../utils.js';

class UIWorker {
    constructor() {
        this.currentState = AppState.INIT;
        this.logBuffer = [];
        this.statsBuffer = null;
        this.lastLogFlush = 0;
        this.lastStatsFlush = 0;
        this.flushInterval = 50; // Reduced from 16ms to 50ms (~20fps)
        this.timeOrigin = performance.timeOrigin;

        // Speed history for graph (store last N points)
        this.speedHistory = [];
        this.maxHistoryPoints = 100; // Reduced from 200 to 100

        this.bufferLimits = {
            maxLogBuffer: 50, // Reduced from 100
            logFlushInterval: 200,  // Reduced frequency from 33ms to 200ms (5fps)
            statsFlushInterval: 200 // Reduced frequency from 100ms to 200ms (5fps)
        };

        this.setupMessageHandler();
        this.startFlushLoop();
    }

    setupMessageHandler() {
        self.onmessage = (e) => {
            const { type, data, state, reason } = e.data;

            if (type === 'state_change') {
                this.handleStateChange(state, reason, data);
            }

            switch (type) {
                case 'log':
                    this.addLog(data);
                    break;
                case 'stats':
                    this.updateStats(data);
                    // Force immediate flush if this is a final progress update
                    if (data.final === true) {
                        this.flushStats();
                    }
                    break;
                case 'state_change':
                    // Already handled above
                    break;
                default:
                    console.warn('UI Worker: Unknown message type', type);
                    break;
            }
        };
    }

    handleStateChange(newState, reason, data) {
        this.currentState = newState;
        this.adjustBufferBehavior(newState);

        if (newState === AppState.WARMUP) {
            // Reset speed history for new tests
            this.speedHistory = [];
        }

        if (newState === AppState.STOPPING) {
            this.flushStats();
        }
    }

    adjustBufferBehavior(state) {
        switch (state) {
            case AppState.RUNNING:
            case AppState.STOPPING:
                this.bufferLimits.logFlushInterval = 200;    // 5fps
                this.bufferLimits.statsFlushInterval = 200;  // 5fps
                break;
            default:
                this.bufferLimits.logFlushInterval = 500;   // 2fps
                this.bufferLimits.statsFlushInterval = 500; // 2fps
                break;
        }
    }

    addLog(logData) {
        const logEntry = {
            ...logData,
            timestamp: logData.timestamp || (performance.now() + this.timeOrigin),
            state: this.currentState
        };

        this.logBuffer.push(logEntry);

        if (this.logBuffer.length > this.bufferLimits.maxLogBuffer) {
            this.logBuffer = this.logBuffer.slice(-this.bufferLimits.maxLogBuffer);
        }

        if (logEntry.className === 'error') {
            this.flushLogs();
        }
    }

    updateStats(statsData) {
        // statsData: { totalBytes, elapsedMs }
        const bytesPerSec = statsData.elapsedMs > 0
            ? (statsData.totalBytes / (statsData.elapsedMs / 1000))
            : 0;

        const enriched = {
            ...statsData,
            timestamp: performance.now(),
            state: this.currentState,
            speeds: this.formatSpeeds(bytesPerSec)
        };

        // Maintain speed history for graph
        this.speedHistory.push({ time: enriched.timestamp, speedBps: bytesPerSec });
        if (this.speedHistory.length > this.maxHistoryPoints) {
            this.speedHistory.shift();
        }

        enriched.speedHistory = [...this.speedHistory];
        this.statsBuffer = enriched;
    }

    formatSpeeds(bytesPerSec) {
        // For network speeds (bits), use 1000-based units (SI)
        const bitsPerSec = bytesPerSec * 8;
        
        // For file sizes (bytes), use 1024-based units (binary)
        return {
            // File size units (1024-based)
            Bps: bytesPerSec,
            KBps: bytesPerSec / 1024,
            MBps: bytesPerSec / (1024 * 1024),
            GBps: bytesPerSec / (1024 * 1024 * 1024),
            
            // Network units (1000-based)
            kbps: bitsPerSec / 1000,
            Mbps: bitsPerSec / 1e6,
            Gbps: bitsPerSec / 1e9
        };
    }

    startFlushLoop() {
        setInterval(() => {
            this.processBuffers();
        }, this.flushInterval);
    }

    processBuffers() {
        const now = performance.now();

        if (this.shouldFlushLogs(now)) {
            this.flushLogs();
        }

        if (this.shouldFlushStats(now)) {
            this.flushStats();
        }
    }

    shouldFlushLogs(now) {
        return this.logBuffer.length > 0 &&
            (now - this.lastLogFlush > this.bufferLimits.logFlushInterval ||
                this.logBuffer.length >= this.bufferLimits.maxLogBuffer);
    }

    shouldFlushStats(now) {
        if (this.currentState === AppState.STOPPING) {
            return this.statsBuffer !== null;
        }
        return this.statsBuffer !== null &&
            (now - this.lastStatsFlush > this.bufferLimits.statsFlushInterval);
    }

    flushLogs() {
        if (this.logBuffer.length === 0) return;

        const logsToFlush = [...this.logBuffer];
        this.logBuffer = [];

        if (logsToFlush.length > 1) {
            logsToFlush.sort((a, b) => a.timestamp - b.timestamp);
        }

        const enrichedLogs = logsToFlush.map(log => ({
            ...log,
            stateContext: this.currentState
        }));

        this.postMessage('logs_rendered', { logs: enrichedLogs });
        this.lastLogFlush = performance.now();
    }

    flushStats() {
        if (this.statsBuffer === null) return;

        const statsToFlush = { ...this.statsBuffer };
        this.statsBuffer = null;

        this.postMessage('stats_rendered', { stats: statsToFlush });
        this.lastStatsFlush = performance.now();
    }

    postMessage(type, data = {}) {
        self.postMessage({ type, data });
    }
}

// Initialize worker
const worker = new UIWorker();