import { AppState } from '../utils.js';

class DownloadWorker {
    constructor() {
        this.transport = null;
        this.reader = null;
        this.currentState = AppState.INIT;
        this.workerId = null;

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
            // Build URL with query parameters instead of sending control message
            const url = new URL(this.config.url);
            url.searchParams.set('size', this.config.sizeBytes.toString());
            url.searchParams.set('threads', '2'); // Default to 2 threads
            
            this.postLog(`Connecting to ${url.href}`);

            this.transport = new WebTransport(url.href, {
                allowPooling: false,
                requireUnreliable: true,
                congestionControl: 'throughput'
            });

            await this.transport.ready;

            // Start receiving data immediately (no control message needed)
            this.stats.startTime = performance.now();
            await this.receiveData();

            const mbSI = (this.stats.bytesReceived / 1000000).toFixed(2);
            const mibIEC = (this.stats.bytesReceived / (1024 * 1024)).toFixed(2);
            this.postLog(`Download complete: ${mbSI} MB (SI) / ${mibIEC} MiB (IEC)`);
            
            // Close the transport
            await this.transport.close();
            
            // Wait for the transport to be fully closed
            try {
                await this.transport.closed;
            } catch (err) {
                // Ignore normal close errors
                if (err.message && !err.message.toLowerCase().includes('connection lost')) {
                    this.postLog(`Close warning: ${err.message}`, 'warning');
                }
            }
            
            // Finally send the done message after everything is complete
            this.postMessage('done', { workerId: this.workerId });
            
            // Handle graceful connection closure after everything else
            this.transport.closed
                .then(() => {
                    this.postLog(`Connection closed gracefully`);
                })
                .catch((err) => {
                    // Don't treat normal close as an error
                    if (err.message && err.message.toLowerCase().includes('connection lost')) {
                        this.postLog(`Connection closed normally`);
                    } else {
                        this.postLog(`Connection closed with error: ${err.message}`, 'error');
                    }
                });
        } catch (err) {
            this.postLog(`Error: ${err.message}`, 'error');
            
            this.postMessage('done', { workerId: this.workerId });
        }
    }

    async receiveData() {
        try {
            const reader = this.transport.incomingUnidirectionalStreams.getReader();
            let dataStream = null;
            
            try {
                const { value, done } = await reader.read();
                if (done) {
                    this.postLog(`No data stream available`, 'warning');
                    return;
                }
                dataStream = value;
            } catch (err) {
                this.postLog(`Error getting data stream: ${err.message}`, 'error');
                return;
            }
            
            if (!dataStream) {
                this.postLog(`No data stream received`, 'error');
                return;
            }

            this.reader = dataStream.getReader();
            
            // Keep reading until we get 'done'
            let bytesReadInChunk = 0;
            let lastProgressTime = performance.now();
            const PROGRESS_INTERVAL = 100; // ms
            
            try {
                while (true) {
                    const { value, done } = await this.reader.read();
                    
                    if (done) {
                        this.postLog(`Stream completed, received ${(this.stats.bytesReceived / (1024 * 1024)).toFixed(2)} MiB total`);
                        break;
                    }
                    
                    // Update local stats
                    this.stats.bytesReceived += value.byteLength;
                    bytesReadInChunk += value.byteLength;
                    
                    // Report progress at most every 100ms to reduce overhead
                    const now = performance.now();
                    if (now - lastProgressTime > PROGRESS_INTERVAL) {
                        this.postMessage('progress', {
                            workerId: this.workerId,
                            bytes: bytesReadInChunk
                        });
                        bytesReadInChunk = 0;
                        lastProgressTime = now;
                    }
                }
                
                // Send any remaining bytes in the final progress update
                if (bytesReadInChunk > 0) {
                    this.postMessage('progress', {
                        workerId: this.workerId,
                        bytes: bytesReadInChunk,
                        final: true
                    });
                } else {
                    // Send final progress even if no bytes left to ensure UI gets final update
                    this.postMessage('progress', {
                        workerId: this.workerId,
                        bytes: 0,
                        final: true
                    });
                }
                
                // Wait a moment to ensure the main thread processes the final progress
                await new Promise(resolve => setTimeout(resolve, 50));
            } catch (err) {
                // Log error but don't throw, so we can still report received bytes
                if (err.message && err.message.toLowerCase().includes('connection lost')) {
                    this.postLog(`Stream ended early, received ${(this.stats.bytesReceived / (1024 * 1024)).toFixed(2)} MiB`);
                } else {
                    this.postLog(`Error receiving data: ${err.message}`, 'error');
                }
            }
        } catch (err) {
            this.postLog(`Error setting up data stream: ${err.message}`, 'error');
            throw err;
        }
    }

    async stopDownload() {
        try {
            if (this.reader) {
                await this.reader.cancel();
                this.reader = null;
            }
            if (this.transport) {
                await this.transport.close();
                this.transport = null;
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
const worker = new DownloadWorker();