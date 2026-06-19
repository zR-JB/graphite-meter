import { StateManager, AppState, createStateMessage } from './state.js';

class WebTransportPingUtility {
    constructor() {
        this.stateManager = new StateManager(AppState.INIT);
        this.transportWorker = null;
        this.uiWorker = null;
        this.isInitializingWorkers = false;
        this.isDisconnecting = false;
        
        this.stats = {
            sent: 0,
            received: 0,
            latencies: [],
            startTime: null,
            lastUpdate: 0
        };
        
        this.config = {
            interval: 10,
            url: '',
            gracePeriodMs: 500
        };

        this.initializeStateListeners();
        this.initializeUI();
    }

    initializeStateListeners() {
        this.stateManager.addListener((stateChange) => {
            this.updateUIForState(stateChange.to);
            this.broadcastStateToWorkers(stateChange);
            this.logStateTransition(stateChange);
        });
    }

    logStateTransition(stateChange) {
        const { to, reason, data } = stateChange;
        
        switch(to) {
            case AppState.CONNECTING:
                this.logMessage(`Connecting to ${data?.url || 'server'}...`, 'info');
                break;
            case AppState.CONNECTED:
                this.logMessage(`Connected to ${data?.url || 'server'}`, 'success');
                break;
            case AppState.WARMUP:
                this.logMessage(`Starting benchmark...`, 'info');
                break;
            case AppState.RUNNING:
                const interval = data?.interval || this.config.interval;
                this.logMessage(`Benchmark started: ${interval}ms interval`, 'success');
                break;
            case AppState.STOPPING:
                this.logMessage(`Benchmark stopping, collecting final responses...`, 'warning');
                break;
            case AppState.STOPPED_WITH_RESULTS:
                this.logMessage('Benchmark stopped', 'success');
                break;
            case AppState.RECOVERING:
                this.logMessage(`Connection lost, attempting recovery...`, 'warning');
                break;
            case AppState.CRASHED:
                if (reason.includes('Connection failed')) {
                    this.logMessage(`Connection failed: ${data?.error || 'Unknown error'}`, 'error');
                } else if (reason.includes('recovery failed')) {
                    this.logMessage('Connection recovery failed', 'error');
                } else {
                    this.logMessage(`Error: ${reason}`, 'error');
                }
                break;
            case AppState.DISCONNECTING:
                this.logMessage('Disconnecting...', 'info');
                break;
            case AppState.INIT:
                if (stateChange.from !== null) { // Not initial state
                    this.logMessage('Disconnected', 'success');
                }
                break;
        }
    }

    initializeUI() {
        const currentUrl = new URL(window.location.href);
        const defaultUrl = `${currentUrl.protocol}//${currentUrl.host}/api/ping`;
        document.getElementById('url').value = defaultUrl;
        this.config.url = defaultUrl;
        this.updateUIForState(AppState.INIT);
    }

    async initializeWorkers() {
        try {
            if (this.isInitializingWorkers) {
                while (this.isInitializingWorkers) {
                    await new Promise(resolve => setTimeout(resolve, 10));
                }
                return true;
            }
            
            this.isInitializingWorkers = true;
            
            if (this.transportWorker) {
                this.transportWorker.terminate();
                this.transportWorker = null;
            }
            if (this.uiWorker) {
                this.uiWorker.terminate();
                this.uiWorker = null;
            }
            
            this.transportWorker = new Worker('./workers/transport-worker.js', { type: 'module' });
            this.uiWorker = new Worker('./workers/ui-worker.js', { type: 'module' });

            this.transportWorker.onmessage = (e) => this.handleTransportMessage(e);
            this.transportWorker.onerror = (error) => {
                console.error('[MAIN] Transport worker error:', error);
                this.stateManager.transition(AppState.CRASHED, 'Transport worker error');
            };

            this.uiWorker.onmessage = (e) => this.handleUIMessage(e);
            this.uiWorker.onerror = (error) => {
                console.error('[MAIN] UI worker error:', error);
            };

            this.broadcastStateToWorkers({
                to: this.stateManager.getState(),
                from: null,
                reason: 'initialization',
                timestamp: performance.now()
            });

            this.isInitializingWorkers = false;
            return true;
        } catch (error) {
            console.error('[MAIN] Failed to initialize workers:', error);
            this.isInitializingWorkers = false;
            return false;
        }
    }

    handleTransportMessage(e) {
        const { type, data, state, reason } = e.data;
        
        if (type === 'state_change') {
            this.stateManager.transition(state, reason);
            return;
        }
        
        switch(type) {
            case 'ping_sent':
                this.stats.sent = data.sent;
                this.logMessage(`→ PING ${data.uuid}`, 'ping-sent', data.sendTime);
                this.scheduleStatsUpdate();
                break;
                
            case 'pong_received':
                this.stats.received = data.received;
                const latency = data.receiveTime - data.sendTime;
                this.stats.latencies.push(latency);
                this.logMessage(`← PONG ${data.uuid} (${latency.toFixed(3)}ms)`, 'pong-received', data.receiveTime);
                this.scheduleStatsUpdate();
                break;
                
            case 'error':
                this.logMessage(`Error: ${data.error}`, 'error');
                break;
                
            case 'recovery_attempt':
                this.logMessage(`Recovery attempt ${data.attempt}: ${data.message}`, 'warning');
                break;
                
            default:
                console.warn('[MAIN] Unknown transport message type:', type);
        }
    }

    handleUIMessage(e) {
        const { type, data } = e.data;
        
        switch(type) {
            case 'logs_rendered':
                this.displayLogs(data.logs);
                break;
                
            case 'stats_rendered':
                this.updateStatsDisplay(data.stats);
                break;
        }
    }

    broadcastStateToWorkers(stateChange) {
        const message = createStateMessage(stateChange.to, stateChange.reason, stateChange.data);
        
        if (this.transportWorker) {
            this.transportWorker.postMessage(message);
        }
        
        if (this.uiWorker) {
            this.uiWorker.postMessage(message);
        }
    }

    updateUIForState(state) {
        const connectBtn = document.querySelector('button[onclick="app.connect()"]');
        const disconnectBtn = document.getElementById('disconnect-btn');
        const startBtn = document.getElementById('start-btn');
        const stopBtn = document.getElementById('stop-btn');
        const urlInput = document.getElementById('url');
        const intervalInput = document.getElementById('interval');
        
        connectBtn.disabled = false;
        disconnectBtn.disabled = true;
        startBtn.disabled = true;
        stopBtn.disabled = true;
        urlInput.disabled = false;
        intervalInput.disabled = false;
        stopBtn.textContent = 'Stop Test';
        
        this.updateStatusDisplay(state);
        
        switch(state) {
            case AppState.INIT:
                break;
                
            case AppState.CONNECTING:
                connectBtn.disabled = true;
                urlInput.disabled = true;
                break;
                
            case AppState.CONNECTED:
                connectBtn.disabled = true;
                disconnectBtn.disabled = false;
                startBtn.disabled = false;
                urlInput.disabled = true;
                break;
                
            case AppState.WARMUP:
            case AppState.RUNNING:
                connectBtn.disabled = true;
                disconnectBtn.disabled = false;
                startBtn.disabled = true;
                stopBtn.disabled = false;
                urlInput.disabled = true;
                intervalInput.disabled = true;
                break;
                
            case AppState.STOPPING:
                connectBtn.disabled = true;
                disconnectBtn.disabled = false;
                startBtn.disabled = true;
                stopBtn.disabled = true;
                stopBtn.textContent = 'Stopping...';
                urlInput.disabled = true;
                intervalInput.disabled = true;
                break;
                
            case AppState.STOPPED_WITH_RESULTS:
                connectBtn.disabled = true;
                disconnectBtn.disabled = false;
                startBtn.disabled = false;
                stopBtn.disabled = true;
                urlInput.disabled = true;
                intervalInput.disabled = false;
                break;
                
            case AppState.RECOVERING:
                connectBtn.disabled = true;
                disconnectBtn.disabled = false;
                startBtn.disabled = true;
                stopBtn.disabled = false;
                urlInput.disabled = true;
                intervalInput.disabled = true;
                break;
                
            case AppState.CRASHED:
                disconnectBtn.disabled = false;
                startBtn.disabled = true;
                stopBtn.disabled = true;
                break;
                
            case AppState.DISCONNECTING:
                connectBtn.disabled = true;
                disconnectBtn.disabled = true;
                startBtn.disabled = true;
                stopBtn.disabled = true;
                urlInput.disabled = true;
                intervalInput.disabled = true;
                break;
        }
    }

    updateStatusDisplay(state) {
        const statusMap = {
            [AppState.INIT]: { text: 'Disconnected', class: 'init' },
            [AppState.CONNECTING]: { text: 'Connecting...', class: 'connecting' },
            [AppState.CONNECTED]: { text: 'Connected', class: 'connected' },
            [AppState.WARMUP]: { text: 'Starting...', class: 'connecting' },
            [AppState.RUNNING]: { text: 'Running Test', class: 'running' },
            [AppState.STOPPING]: { text: 'Stopping Test...', class: 'stopping' },
            [AppState.STOPPED_WITH_RESULTS]: { text: 'Test Completed', class: 'completed' },
            [AppState.RECOVERING]: { text: 'Recovering...', class: 'recovering' },
            [AppState.CRASHED]: { text: 'Error', class: 'error' },
            [AppState.DISCONNECTING]: { text: 'Disconnecting...', class: 'stopping' }
        };

        const status = statusMap[state] || { text: 'Unknown', class: 'init' };
        
        if (window.updateStatus) {
            window.updateStatus(status.text, status.class);
        }
    }

    async connect() {
        if (!this.stateManager.canConnect()) {
            this.logMessage('Cannot connect in current state', 'error');
            return;
        }
        
        if (this.isInitializingWorkers) {
            this.logMessage('Connection attempt already in progress', 'warning');
            return;
        }
        
        if (this.isDisconnecting) {
            this.logMessage('Cannot connect while disconnecting, please wait', 'warning');
            return;
        }

        const url = document.getElementById('url').value.trim();
        if (!url) {
            this.logMessage('Please enter a valid URL', 'error');
            return;
        }

        this.config.url = url;
        
        if (!await this.initializeWorkers()) {
            this.logMessage('Failed to initialize workers', 'error');
            return;
        }

        await this.stateManager.transition(AppState.CONNECTING, 'User initiated connection', { url });
    }

    async startBenchmark() {
        if (!this.stateManager.canRunBenchmark()) {
            this.logMessage('Cannot start benchmark in current state', 'error');
            return;
        }

        const intervalMs = parseFloat(document.getElementById('interval').value);
        
        if (intervalMs < 4) {
            this.logMessage('Minimum interval is 4ms due to browser limitations', 'error');
            document.getElementById('interval').value = '4';
            return;
        }

        this.config.interval = intervalMs;
        
        await this.stateManager.transition(AppState.WARMUP, 'User started benchmark', { interval: intervalMs });
        
        this.stats.startTime = performance.now();
        this.stats.sent = 0;
        this.stats.received = 0;
        this.stats.latencies = [];
    }

    async stopBenchmark() {
        if (!this.stateManager.canStopBenchmark()) {
            this.logMessage('Cannot stop benchmark in current state', 'error');
            return;
        }

        await this.stateManager.transition(AppState.STOPPING, 'User requested benchmark stop');
        
        this.scheduleStatsUpdate();
        
        const stopUpdateInterval = setInterval(() => {
            if (this.stateManager.isState(AppState.STOPPING)) {
                this.scheduleStatsUpdate();
            } else {
                clearInterval(stopUpdateInterval);
            }
        }, 100);
    }

    async disconnect() {
        if (!this.stateManager.canDisconnect()) {
            this.logMessage('Cannot disconnect in current state', 'error');
            return;
        }

        if (this.isDisconnecting) {
            return;
        }
        
        this.isDisconnecting = true;

        await this.stateManager.transition(AppState.DISCONNECTING, 'User initiated disconnect');
        
        await new Promise(resolve => setTimeout(resolve, 500));
        
        if (this.transportWorker) {
            this.transportWorker.terminate();
            this.transportWorker = null;
        }
        if (this.uiWorker) {
            this.uiWorker.terminate();
            this.uiWorker = null;
        }
        
        this.isInitializingWorkers = false;
        this.isDisconnecting = false;
        
        await this.stateManager.transition(AppState.INIT, 'Disconnected');
    }

    logMessage(message, className = 'info', timestamp = null) {
        if (this.uiWorker) {
            this.uiWorker.postMessage({
                type: 'log',
                data: { message, className, timestamp }
            });
        }
    }

    scheduleStatsUpdate() {
        const now = performance.now();
        if (this.stateManager.isState(AppState.STOPPING) || now - this.stats.lastUpdate > 100) {
            const stats = {
                sent: this.stats.sent,
                received: this.stats.received,
                rate: this.calculateRate(),
                packetLoss: this.stats.sent > 0 ? 
                    ((this.stats.sent - this.stats.received) / this.stats.sent * 100) : 0,
                latencies: [...this.stats.latencies]
            };
            
            if (this.uiWorker) {
                this.uiWorker.postMessage({
                    type: 'stats',
                    data: stats
                });
            }
            
            this.stats.lastUpdate = now;
        }
    }

    calculateRate() {
        if (!this.stateManager.isCollectingData() || !this.stats.startTime) {
            return 0;
        }
        
        const elapsed = (performance.now() - this.stats.startTime) / 1000;
        return elapsed > 0 ? (this.stats.received / elapsed) : 0;
    }

    displayLogs(logs) {
        const logDiv = document.getElementById('log');
        const fragment = document.createDocumentFragment();
        
        if (logs.length > 1) {
            logs.sort((a, b) => a.timestamp - b.timestamp);
        }
        
        logs.forEach(log => {
            const div = document.createElement('div');
            div.className = log.className;
            
            const timestamp = new Date(log.timestamp);
            const timeStr = timestamp.toLocaleTimeString('de-DE', { 
                hour12: false, 
                hour: '2-digit', 
                minute: '2-digit', 
                second: '2-digit' 
            });
            const ms = (log.timestamp % 1000).toFixed(3).padStart(7, '0');
            
            div.textContent = `[${timeStr}.${ms}] ${log.message}`;
            fragment.appendChild(div);
        });
        
        logDiv.appendChild(fragment);
        
        while (logDiv.childElementCount > 1000) {
            logDiv.removeChild(logDiv.firstChild);
        }
        
        logDiv.scrollTop = logDiv.scrollHeight;
    }

    updateStatsDisplay(stats) {
        document.getElementById('sent').textContent = stats.sent;
        document.getElementById('received').textContent = stats.received;
        document.getElementById('rate').textContent = stats.rate.toFixed(1) + '/s';
        document.getElementById('packet-loss').textContent = stats.packetLoss.toFixed(3) + '%';
        
        if (stats.latencies?.length > 0) {
            document.getElementById('avg').textContent = stats.avg.toFixed(3) + 'ms';
            document.getElementById('min').textContent = stats.min.toFixed(3) + 'ms';
            document.getElementById('max').textContent = stats.max.toFixed(3) + 'ms';
            document.getElementById('p99').textContent = stats.p99.toFixed(3) + 'ms';
        } else {
            document.getElementById('avg').textContent = '-';
            document.getElementById('min').textContent = '-';
            document.getElementById('max').textContent = '-';
            document.getElementById('p99').textContent = '-';
        }
        
        const statsSection = document.querySelector('.stats-section');
        if (this.stateManager.isCollectingData()) {
            statsSection.classList.add('active');
        } else {
            statsSection.classList.remove('active');
        }
    }
}

window.app = new WebTransportPingUtility();
