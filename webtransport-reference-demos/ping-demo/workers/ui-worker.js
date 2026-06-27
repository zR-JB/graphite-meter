import { AppState } from '../state.js';

class UIWorker {
    constructor() {
        this.currentState = AppState.INIT;
        this.logBuffer = [];
        this.statsBuffer = null;
        this.lastLogFlush = 0;
        this.lastStatsFlush = 0;
        this.flushInterval = 16; // ~60fps
        this.timeOrigin = performance.timeOrigin;
        
        this.bufferLimits = {
            maxLogBuffer: 100,
            logFlushInterval: 33,  // 30fps for logs
            statsFlushInterval: 100 // 10fps for stats
        };
        
        this.setupMessageHandler();
        this.startFlushLoop();
    }

    setupMessageHandler() {
        self.onmessage = (e) => {
            const { type, data, state, reason } = e.data;
            
            if (type === 'state_change') {
                this.handleStateChange(state, reason, data);
                return;
            }
            
            switch(type) {
                case 'log':
                    this.addLog(data);
                    break;
                case 'stats':
                    this.updateStats(data);
                    break;
                default:
                    console.warn('[UI] Unknown message type:', type);
            }
        };
    }

    handleStateChange(newState, reason, data) {
        this.currentState = newState;
        this.adjustBufferBehavior(newState);
        
        if(newState === AppState.STOPPING) {
            this.flushStats();
        }
    }

    adjustBufferBehavior(state) {
        switch(state) {
            case AppState.RUNNING:
            case AppState.STOPPING:
                // Higher frequency updates during active states
                this.bufferLimits.logFlushInterval = 33;    // 30fps
                this.bufferLimits.statsFlushInterval = 50;  // 20fps
                break;
                
            default:
                // Standard frequency for other states
                this.bufferLimits.logFlushInterval = 100;   // 10fps
                this.bufferLimits.statsFlushInterval = 200; // 5fps
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
        this.statsBuffer = {
            ...statsData,
            timestamp: performance.now(),
            state: this.currentState
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
        // Always flush in STOPPING state
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
        
        const enrichedStats = this.enrichStats(statsToFlush);
        
        this.postMessage('stats_rendered', { stats: enrichedStats });
        this.lastStatsFlush = performance.now();
    }

    enrichStats(stats) {
        const enriched = { ...stats };
        
        // Add state-specific metrics
        switch(this.currentState) {
            case AppState.RUNNING:
                enriched.status = 'Active';
                enriched.statusClass = 'running';
                break;
                
            case AppState.STOPPING:
                enriched.status = 'Stopping';
                enriched.statusClass = 'stopping';
                break;
                
            case AppState.STOPPED_WITH_RESULTS:
                enriched.status = 'Completed';
                enriched.statusClass = 'completed';
                break;
                
            case AppState.RECOVERING:
                enriched.status = 'Recovering';
                enriched.statusClass = 'recovering';
                break;
                
            case AppState.CRASHED:
                enriched.status = 'Error';
                enriched.statusClass = 'error';
                break;
                
            default:
                enriched.status = 'Idle';
                enriched.statusClass = 'idle';
                break;
        }
        
        // Calculate latency metrics if we have data
        if (enriched.latencies?.length > 0) {
            const sorted = [...enriched.latencies].sort((a, b) => a - b);
            const len = sorted.length;
            
            enriched.min = sorted[0];
            enriched.max = sorted[len - 1];
            
            const sum = sorted.reduce((acc, val) => acc + val, 0);
            enriched.avg = sum / len;
            enriched.p99 = sorted[Math.floor(len * 0.99)] || 0;
            enriched.latencies = sorted;
        } else {
            enriched.min = 0;
            enriched.max = 0;
            enriched.avg = 0;
            enriched.p99 = 0;
        }
        
        return enriched;
    }

    postMessage(type, data = {}) {
        self.postMessage({ type, data });
    }
}

// Initialize worker
const worker = new UIWorker();
