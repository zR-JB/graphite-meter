import {
  StateManager,
  AppState,
  createStateMessage,
  formatBytes,
  formatBits,
} from './utils.js';

class WebTransportUploadClient {
  constructor() {
    this.stateManager = new StateManager(AppState.INIT);
    this.uploadWorkers = [];
    this.uiWorker = null;
    this.isInitializingWorkers = false;
    this.isDisconnecting = false;
    this.activeWorkers = 0;

    this.config = {
      url: '',
      sizeBytes: 0,
      workers: 1,
      measurementMode: 'client',
      transportMode: 'webtransport'
    };

    this.stats = {
      totalBytes: 0,
      startTime: null,
    };

    this.initializeStateListeners();
    this.initializeUI();
  }

  initializeStateListeners() {
    this.stateManager.addListener((stateChange) => {
      this.updateUIForState(stateChange.to);
      this.updateStatusDisplay(stateChange.to);
      this.broadcastStateToWorkers(stateChange);
      this.logStateTransition(stateChange);
    });
  }

  logStateTransition(stateChange) {
    const { to, reason, data } = stateChange;
    switch (to) {
      case AppState.CONNECTING:
        this.logMessage(`Connecting to ${data?.url || 'server'}...`, 'info');
        break;
      case AppState.CONNECTED:
        this.logMessage(`Connected to ${data?.url || 'server'}`, 'success');
        break;
      case AppState.WARMUP:
        this.logMessage(`Preparing upload...`, 'info');
        break;
      case AppState.RUNNING:
        this.logMessage(
          `Upload started with ${this.config.workers} worker(s)`,
          'success'
        );
        break;
      case AppState.STOPPING:
        this.logMessage(`Stopping upload...`, 'warning');
        break;
      case AppState.STOPPED_WITH_RESULTS:
        this.logMessage('Upload stopped', 'success');
        break;
      case AppState.RECOVERING:
        this.logMessage(`Connection lost, attempting recovery...`, 'warning');
        break;
      case AppState.CRASHED:
        this.logMessage(`Error: ${reason}`, 'error');
        break;
      case AppState.DISCONNECTING:
        this.logMessage('Disconnecting...', 'info');
        break;
      case AppState.INIT:
        if (stateChange.from !== null) {
          this.logMessage('Disconnected', 'success');
        }
        break;
    }
  }

  initializeUI() {
    const currentUrl = new URL(window.location.href);
    
    // Set default URLs based on transport mode
    this.updateUrlForTransportMode();
    this.updateUILabelsForTransportMode();
    
    this.updateUIForState(AppState.INIT);

    // Bind buttons
    document.getElementById('start-test-btn').onclick = () => this.startTest();
    document.getElementById('disconnect-btn').onclick = () => this.disconnect();
    document.getElementById('stop-btn').onclick = () => this.stopUpload();
    
    // Bind transport mode radio buttons
    document.querySelectorAll('input[name="transport"]').forEach(radio => {
      radio.addEventListener('change', () => {
        this.config.transportMode = radio.value;
        this.updateUrlForTransportMode();
        this.updateUILabelsForTransportMode();
      });
    });
  }

  updateUrlForTransportMode() {
    const currentUrl = new URL(window.location.href);
    const transportMode = document.querySelector('input[name="transport"]:checked')?.value || 'webtransport';
    
    let defaultUrl;
    if (transportMode === 'webtransport') {
      defaultUrl = `${currentUrl.protocol}//${currentUrl.host}/api/upload`;
    } else {
      defaultUrl = `${currentUrl.protocol}//${currentUrl.host}/upload`;
    }
    
    document.getElementById('url').value = defaultUrl;
    this.config.url = defaultUrl;
    this.config.transportMode = transportMode;
  }

  updateUILabelsForTransportMode() {
    const transportMode = this.config.transportMode;
    const workersInput = document.getElementById('workers');
    const workersLabel = document.querySelector('label[for="workers"]');
    const modeInput = document.getElementById('measurement-mode');
    
    if (transportMode === 'fetch') {
      // For fetch mode, multiple workers means multiple parallel requests
      workersLabel.textContent = 'Parallel Requests:';
      workersInput.max = '8'; // Reasonable limit for HTTP requests
      // Disable measurement mode for fetch (always client-based)
      modeInput.value = 'client';
      modeInput.disabled = true;
    } else {
      // For WebTransport mode, workers are WebTransport connections
      workersLabel.textContent = 'Workers:';
      workersInput.max = '16';
      // Enable measurement mode for WebTransport
      modeInput.disabled = false;
    }
  }

  async initializeWorkers() {
    if (this.isInitializingWorkers) {
      while (this.isInitializingWorkers) {
        await new Promise((r) => setTimeout(r, 10));
      }
      return true;
    }

    this.isInitializingWorkers = true;

    // Terminate old workers
    this.uploadWorkers.forEach((w) => w.terminate());
    this.uploadWorkers = [];
    if (this.uiWorker) {
      this.uiWorker.terminate();
      this.uiWorker = null;
    }

    // Create UI worker
    this.uiWorker = new Worker('./workers/ui-worker.js', { type: 'module' });
    this.uiWorker.onmessage = (e) => this.handleUIMessage(e);
    this.uiWorker.onerror = (err) => console.error('[MAIN] UI worker error:', err);

    // Create upload workers
    for (let i = 0; i < this.config.workers; i++) {
      const workerScript = this.config.transportMode === 'webtransport' 
        ? './workers/upload-worker.js'
        : './workers/fetch-upload-worker.js';
        
      const worker = new Worker(workerScript, { type: 'module' });
      worker.onmessage = (e) => this.handleUploadMessage(e);
      worker.onerror = (err) => {
        console.error(`[MAIN] Upload worker ${i + 1} error:`, err);
        this.stateManager.transition(AppState.CRASHED, `Worker ${i + 1} error`);
      };
      this.uploadWorkers.push(worker);
    }

    // Send initial state to workers
    this.broadcastStateToWorkers({
      to: this.stateManager.getState(),
      from: null,
      reason: 'initialization',
      timestamp: performance.now(),
    });

    this.isInitializingWorkers = false;
    return true;
  }

  handleUploadMessage(e) {
    const { type, data } = e.data;
    if (type === 'progress') {
      // Client-based measurement
      if (this.config.measurementMode === 'client') {
        this.stats.totalBytes += data.bytes;
        const elapsedMs = performance.now() - this.stats.startTime;
        this.uiWorker.postMessage({
          type: 'stats',
          data: {
            totalBytes: this.stats.totalBytes,
            elapsedMs,
            final: data.final || false,
          },
        });
      }
    } else if (type === 'server_progress') {
        this.stats.totalBytes = data.bytes;
        const elapsedMs = performance.now() - this.stats.startTime;
        this.uiWorker.postMessage({
            type: 'stats',
            data: {
            totalBytes: this.stats.totalBytes,
            elapsedMs,
            final: data.final || false,
            },
        });

        if (data.final) {
            // Immediately stop the upload session
            this.logMessage(`Final server measurement received: ${data.bytes} bytes`, 'success');
            this.stateManager.transition(AppState.STOPPED_WITH_RESULTS, 'Server reported upload complete');
            setTimeout(() => this.disconnect(), 500);
        }
    } else if (type === 'log') {
      this.uiWorker.postMessage({ type: 'log', data });
    } else if (type === 'done') {
      this.activeWorkers--;

      // Update the active workers display immediately
      document.getElementById('active-workers').textContent = this.activeWorkers;

      // If all workers are done, transition to completed state
      if (this.activeWorkers <= 0) {
        setTimeout(async () => {
          if (this.stateManager.isState(AppState.RUNNING)) {
            await this.stateManager.transition(
              AppState.STOPPED_WITH_RESULTS,
              'Upload completed'
            );
            setTimeout(() => this.disconnect(), 1000);
          } else if (this.stateManager.isState(AppState.STOPPING)) {
            await this.stateManager.transition(
              AppState.STOPPED_WITH_RESULTS,
              'Upload stopped'
            );
            setTimeout(() => this.disconnect(), 1000);
          }
        }, 200);
      }
    }
  }

  handleUIMessage(e) {
    const { type, data } = e.data;
    if (type === 'logs_rendered') {
      this.displayLogs(data.logs);
    } else if (type === 'stats_rendered') {
      this.updateStatsDisplay(data.stats);
    }
  }

  broadcastStateToWorkers(stateChange) {
    const message = createStateMessage(
      stateChange.to,
      stateChange.reason,
      stateChange.data
    );
    this.uploadWorkers.forEach((w) => w.postMessage(message));
    if (this.uiWorker) {
      this.uiWorker.postMessage(message);
    }
  }

  updateUIForState(state) {
    const startTestBtn = document.getElementById('start-test-btn');
    const disconnectBtn = document.getElementById('disconnect-btn');
    const stopBtn = document.getElementById('stop-btn');
    const urlInput = document.getElementById('url');
    const sizeInput = document.getElementById('size');
    const workersInput = document.getElementById('workers');
    const modeInput = document.getElementById('measurement-mode');
    const transportRadios = document.querySelectorAll('input[name="transport"]');

    startTestBtn.disabled = false;
    disconnectBtn.disabled = true;
    stopBtn.disabled = true;
    urlInput.disabled = false;
    sizeInput.disabled = false;
    workersInput.disabled = false;
    modeInput.disabled = false;
    transportRadios.forEach(radio => radio.disabled = false);

    switch (state) {
      case AppState.INIT:
        updateStatus('Disconnected', 'init');
        break;
      case AppState.CONNECTING:
        updateStatus('Connecting...', 'connecting');
        startTestBtn.disabled = true;
        transportRadios.forEach(radio => radio.disabled = true);
        break;
      case AppState.CONNECTED:
        updateStatus('Connected', 'connected');
        startTestBtn.disabled = true;
        disconnectBtn.disabled = false;
        transportRadios.forEach(radio => radio.disabled = true);
        break;
      case AppState.WARMUP:
        updateStatus('Preparing...', 'connecting');
        startTestBtn.disabled = true;
        disconnectBtn.disabled = false;
        stopBtn.disabled = false;
        urlInput.disabled = true;
        sizeInput.disabled = true;
        workersInput.disabled = true;
        modeInput.disabled = true;
        transportRadios.forEach(radio => radio.disabled = true);
        break;
      case AppState.RUNNING:
        updateStatus('Uploading...', 'running');
        startTestBtn.disabled = true;
        disconnectBtn.disabled = false;
        stopBtn.disabled = false;
        urlInput.disabled = true;
        sizeInput.disabled = true;
        workersInput.disabled = true;
        modeInput.disabled = true;
        transportRadios.forEach(radio => radio.disabled = true);
        break;
      case AppState.STOPPING:
        updateStatus('Stopping...', 'stopping');
        startTestBtn.disabled = true;
        disconnectBtn.disabled = false;
        stopBtn.disabled = true;
        urlInput.disabled = true;
        sizeInput.disabled = true;
        workersInput.disabled = true;
        modeInput.disabled = true;
        transportRadios.forEach(radio => radio.disabled = true);
        break;
      case AppState.STOPPED_WITH_RESULTS:
        updateStatus('Completed', 'completed');
        startTestBtn.disabled = false;
        disconnectBtn.disabled = false;
        transportRadios.forEach(radio => radio.disabled = false);
        break;
      case AppState.RECOVERING:
        updateStatus('Reconnecting...', 'recovering');
        startTestBtn.disabled = true;
        disconnectBtn.disabled = false;
        transportRadios.forEach(radio => radio.disabled = true);
        break;
      case AppState.CRASHED:
        updateStatus('Error', 'error');
        disconnectBtn.disabled = false;
        transportRadios.forEach(radio => radio.disabled = false);
        break;
      case AppState.DISCONNECTING:
        updateStatus('Disconnecting...', 'stopping');
        startTestBtn.disabled = true;
        disconnectBtn.disabled = true;
        stopBtn.disabled = true;
        urlInput.disabled = true;
        sizeInput.disabled = true;
        workersInput.disabled = true;
        modeInput.disabled = true;
        transportRadios.forEach(radio => radio.disabled = true);
        break;
    }
  }

  async startTest() {
    if (!this.stateManager.canConnect()) {
      this.logMessage('Cannot start test in current state', 'error');
      return;
    }

    this.config.url = document.getElementById('url').value.trim();
    this.config.sizeBytes = Math.max(
      1,
      Math.floor(parseFloat(document.getElementById('size').value) * 1000 * 1000)
    );
    this.config.workers = parseInt(
      document.getElementById('workers').value,
      10
    );
    this.config.measurementMode =
      document.getElementById('measurement-mode').value;
    this.config.transportMode = document.querySelector('input[name="transport"]:checked').value;

    if (!(await this.initializeWorkers())) {
      this.logMessage('Failed to initialize workers', 'error');
      return;
    }

    await this.stateManager.transition(AppState.CONNECTING, 'Starting test', {
      url: this.config.url,
    });
    await this.stateManager.transition(AppState.CONNECTED, 'Connected', {
      url: this.config.url,
    });

    // Immediately start upload
    await this.startUpload();
  }

  async startUpload() {
    if (!this.stateManager.canStartUpload()) {
      this.logMessage('Cannot start upload in current state', 'error');
      return;
    }

    const totalSize = Math.max(
      1,
      Math.floor(parseFloat(document.getElementById('size').value) * 1000 * 1000)
    );
    const sizePerWorker = Math.floor(totalSize / this.config.workers);
    const remainder = totalSize % this.config.workers;

    this.stats.totalBytes = 0;
    this.stats.startTime = performance.now();
    this.activeWorkers = this.config.workers;

    await this.stateManager.transition(AppState.WARMUP, 'User started upload');
    await this.stateManager.transition(AppState.RUNNING, 'Upload running');

    this.uploadWorkers.forEach((w, i) => {
      const workerSize =
        sizePerWorker + (i === this.config.workers - 1 ? remainder : 0);
      w.postMessage({
        type: 'start',
        data: {
          url: this.config.url,
          sizeBytes: workerSize,
          workerId: i + 1,
          measurementMode: this.config.measurementMode,
        },
      });
    });
  }

  async stopUpload() {
    if (!this.stateManager.canStopUpload()) {
      this.logMessage('Cannot stop upload in current state', 'error');
      return;
    }

    await this.stateManager.transition(AppState.STOPPING, 'User requested stop');
    this.uploadWorkers.forEach((w) => w.postMessage({ type: 'stop' }));

    if (this.activeWorkers <= 0) {
      await this.stateManager.transition(
        AppState.STOPPED_WITH_RESULTS,
        'Upload stopped'
      );
    }
  }

  async disconnect() {
    if (!this.stateManager.canDisconnect()) {
      this.logMessage('Cannot disconnect in current state', 'error');
      return;
    }

    this.isDisconnecting = true;
    await this.stateManager.transition(
      AppState.DISCONNECTING,
      'User initiated disconnect'
    );

    this.uploadWorkers.forEach((w) => w.terminate());
    this.uploadWorkers = [];
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
        data: { message, className, timestamp },
      });
    }
  }

  displayLogs(logs) {
    const logDiv = document.getElementById('log');
    const fragment = document.createDocumentFragment();

    logs.forEach((log) => {
      const div = document.createElement('div');
      div.className = log.className;
      const timestamp = new Date(log.timestamp);
      const timeStr = timestamp.toLocaleTimeString('de-DE', {
        hour12: false,
        hour: '2-digit',
        minute: '2-digit',
        second: '2-digit',
      });
      const ms = (log.timestamp % 1000).toFixed(3).padStart(7, '0');
      div.textContent = `[${timeStr}.${ms}] ${log.message}`;
      fragment.appendChild(div);
    });

    logDiv.appendChild(fragment);
    while (logDiv.childElementCount > 500) { // Reduced from 1000 to 500
      logDiv.removeChild(logDiv.firstChild);
    }
    logDiv.scrollTop = logDiv.scrollHeight;
  }

  updateStatsDisplay(stats) {
    const totalSI = formatBytes(stats.totalBytes, { base: 1000, decimals: 2 });
    const [valSI, unitSI] = totalSI.split(' ');
    document.getElementById('total-mb').textContent = valSI;
    document
      .querySelector('#total-mb')
      .parentNode.querySelector('.stat-unit').textContent = unitSI;

    const totalIEC = formatBytes(stats.totalBytes, {
      base: 1024,
      decimals: 2,
      iec: true,
    });
    const [valIEC, unitIEC] = totalIEC.split(' ');
    document.getElementById('total-mib').textContent = valIEC;
    document
      .querySelector('#total-mib')
      .parentNode.querySelector('.stat-unit').textContent = unitIEC;

    const bitsPerSec = stats.speeds.Bps * 8;
    const bitsStr = formatBits(bitsPerSec);
    const [bitsVal, bitsUnit] = bitsStr.split(' ');
    document.getElementById('speed-bits').textContent = bitsVal;
    document.getElementById('speed-bits-unit').textContent = bitsUnit;

    const bytesSI = formatBytes(stats.speeds.Bps, { base: 1000, decimals: 2 });
    const [valSI_speed, unitSI_speed] = bytesSI.split(' ');
    document.getElementById('speed-bytes-si').textContent = valSI_speed;
    document.getElementById('speed-bytes-si-unit').textContent =
      unitSI_speed + '/s';

    const bytesIEC = formatBytes(stats.speeds.Bps, {
      base: 1024,
      decimals: 2,
      iec: true,
    });
    const [valIEC_speed, unitIEC_speed] = bytesIEC.split(' ');
    document.getElementById('speed-bytes-iec').textContent = valIEC_speed;
    document.getElementById('speed-bytes-iec-unit').textContent =
      unitIEC_speed + '/s';

    document.getElementById('active-workers').textContent = this.activeWorkers;

    this.drawSpeedGraph(stats.speedHistory);
  }

  updateStatusDisplay(state) {
    const statusMap = {
      [AppState.INIT]: { text: 'Disconnected', class: 'init' },
      [AppState.CONNECTING]: { text: 'Connecting...', class: 'connecting' },
      [AppState.CONNECTED]: { text: 'Connected', class: 'connected' },
      [AppState.WARMUP]: { text: 'Preparing...', class: 'connecting' },
      [AppState.RUNNING]: { text: 'Uploading', class: 'running' },
      [AppState.STOPPING]: { text: 'Stopping...', class: 'stopping' },
      [AppState.STOPPED_WITH_RESULTS]: {
        text: 'Upload Complete',
        class: 'completed',
      },
      [AppState.RECOVERING]: { text: 'Recovering...', class: 'recovering' },
      [AppState.CRASHED]: { text: 'Error', class: 'error' },
      [AppState.DISCONNECTING]: { text: 'Disconnecting...', class: 'stopping' },
    };
    const status = statusMap[state] || { text: 'Unknown', class: 'init' };
    if (window.updateStatus) {
      window.updateStatus(status.text, status.class);
    }
  }

  drawSpeedGraph(history) {
    const canvas = document.getElementById('speed-graph');
    if (!canvas) return;
    const ctx = canvas.getContext('2d');
    const w = canvas.width;
    const h = canvas.height;

    ctx.clearRect(0, 0, w, h);

    if (history.length < 2) return;

    const maxSpeed = Math.max(...history.map((p) => p.speedBps)) || 1;
    ctx.beginPath();
    ctx.strokeStyle = '#0f0';
    ctx.lineWidth = 2;

    history.forEach((point, i) => {
      const x = (i / (history.length - 1)) * w;
      const y = h - (point.speedBps / maxSpeed) * h;
      if (i === 0) ctx.moveTo(x, y);
      else ctx.lineTo(x, y);
    });

    ctx.stroke();
  }
}

window.app = new WebTransportUploadClient();