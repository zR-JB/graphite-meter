import { AppState } from '../utils.js';

class FetchDownloadWorker {
    constructor() {
        this.currentState = AppState.INIT;
        this.workerId = null;
        this.abortController = null;

        this.config = {
            url: '',
            sizeBytes: 0
        };

        this.stats = {
            bytesReceived: 0,
            startTime: null
        };

        this.setupMessageHandler();
    }

    setupMessageHandler() {
        self.onmessage = (e) => {
            const { type, data, state, reason } = e.data;

            if (type === 'state_change') {
                this.handleStateChange(state, reason, data);
            } else if (type === 'start') {
                this.workerId = data.workerId;
                this.config.url = data.url;
                this.config.sizeBytes = data.sizeBytes;
                this.startDownload();
            } else if (type === 'stop') {
                this.stopDownload();
            }
        };
    }

    handleStateChange(newState, reason, data) {
        this.currentState = newState;
    }

    async startDownload() {
        try {
            // Build URL with query parameters
            const url = new URL(this.config.url);
            url.searchParams.set('size', this.config.sizeBytes.toString());
            
            this.postLog(`Starting HTTP fetch from ${url.href}`);

            this.abortController = new AbortController();
            this.stats.startTime = performance.now();

            const response = await fetch(url.href, {
                method: 'GET',
                signal: this.abortController.signal
            });

            if (!response.ok) {
                throw new Error(`HTTP ${response.status}: ${response.statusText}`);
            }

            // Process the response stream efficiently without memory accumulation
            let bytesReadInInterval = 0;
            const PROGRESS_INTERVAL = 100; // ms - fixed interval
            
            // Start progress reporting timer
            const progressTimer = setInterval(() => {
                if (bytesReadInInterval > 0) {
                    this.postMessage('progress', {
                        workerId: this.workerId,
                        bytes: bytesReadInInterval
                    });
                    bytesReadInInterval = 0;
                }
            }, PROGRESS_INTERVAL);
            
            try {
                // Use pipeTo with a custom WritableStream for maximum efficiency
                const downloadWorker = this;
                await response.body.pipeTo(new WritableStream({
                    write(chunk) {
                        // Just count bytes, don't store them to avoid memory issues
                        this.totalBytesReceived += chunk.byteLength;
                        bytesReadInInterval += chunk.byteLength;
                    },
                    start() {
                        this.totalBytesReceived = 0;
                    },
                    close() {
                        // Stream completed successfully
                        clearInterval(progressTimer);
                        
                        // Send final progress
                        if (bytesReadInInterval > 0) {
                            downloadWorker.postMessage('progress', {
                                workerId: downloadWorker.workerId,
                                bytes: bytesReadInInterval,
                                final: true
                            });
                        } else {
                            downloadWorker.postMessage('progress', {
                                workerId: downloadWorker.workerId,
                                bytes: 0,
                                final: true
                            });
                        }
                        
                        downloadWorker.stats.bytesReceived = this.totalBytesReceived;
                        downloadWorker.postLog(`Download completed: ${(this.totalBytesReceived / (1024 * 1024)).toFixed(2)} MiB total`);
                    },
                    abort(reason) {
                        clearInterval(progressTimer);
                        downloadWorker.postLog(`Download aborted: ${reason}`);
                    }
                }), {
                    signal: this.abortController.signal
                });
                
            } catch (err) {
                clearInterval(progressTimer);
                if (err.name === 'AbortError') {
                    this.postLog(`Download stopped by user`);
                } else {
                    this.postLog(`Error reading response: ${err.message}`, 'error');
                }
            }

            const mbSI = (this.stats.bytesReceived / 1000000).toFixed(2);
            const mibIEC = (this.stats.bytesReceived / (1024 * 1024)).toFixed(2);
            this.postLog(`Download complete: ${mbSI} MB (SI) / ${mibIEC} MiB (IEC)`);
            
            // Finally send the done message
            this.postMessage('done', { workerId: this.workerId });
            
        } catch (err) {
            if (err.name === 'AbortError') {
                this.postLog(`Download cancelled`);
            } else {
                this.postLog(`Error: ${err.message}`, 'error');
            }
            
            this.postMessage('done', { workerId: this.workerId });
        }
    }

    async stopDownload() {
        try {
            if (this.abortController) {
                this.abortController.abort();
                this.abortController = null;
            }
            this.postLog(`Download stopped by user`);
        } catch (err) {
            this.postLog(`Error stopping download: ${err.message}`, 'error');
        }
    }

    postLog(message, className = 'info') {
        this.postMessage('log', { workerId: this.workerId, message, className });
    }

    postMessage(type, data = {}) {
        self.postMessage({ type, data });
    }
}

// Initialize worker
const worker = new FetchDownloadWorker();
