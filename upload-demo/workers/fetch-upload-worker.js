import init, { ScrambledCounterRng } from './pkg/rngwasm.js';
import { AppState } from '../utils.js';

class FetchUploadWorker {
    constructor() {
        this.currentState = AppState.INIT;
        this.workerId = null;
        this.abortController = null;

        this.config = {
            url: '',
            sizeBytes: 0
        };

        this.stats = {
            bytesSent: 0,
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
                this.startUpload();
            } else if (type === 'stop') {
                this.stopUpload();
            }
        };
    }

    handleStateChange(newState, reason, data) {
        this.currentState = newState;
    }

    async startUpload() {
        try {
            // Build URL for HTTP upload
            const url = new URL(this.config.url);
            
            this.postLog(`Starting HTTP upload to ${url.href}`);

            // Initialize RNG
            await init();
            const rng = new ScrambledCounterRng(
                this.randomBigInt64(),
                this.randomBigInt64()
            );

            this.abortController = new AbortController();
            this.stats.startTime = performance.now();

            const totalSize = this.config.sizeBytes;
            let bytesSentInInterval = 0;
            const PROGRESS_INTERVAL = 100; // ms - fixed interval
            const CHUNK_SIZE = 64 * 1024; // 64KB chunks for smooth streaming

            // Start progress reporting timer
            const progressTimer = setInterval(() => {
                if (bytesSentInInterval > 0) {
                    this.postMessage('progress', {
                        workerId: this.workerId,
                        bytes: bytesSentInInterval
                    });
                    bytesSentInInterval = 0;
                }
            }, PROGRESS_INTERVAL);

            // Create a high-performance readable stream similar to the benchmark
            const stream = new ReadableStream({
                start(controller) {
                    this.sentBytes = 0;
                },
                
                pull(controller) {
                    if (uploadWorker.abortController?.signal.aborted || this.sentBytes >= totalSize) {
                        clearInterval(progressTimer);
                        
                        // Send final progress
                        if (bytesSentInInterval > 0) {
                            uploadWorker.postMessage('progress', {
                                workerId: uploadWorker.workerId,
                                bytes: bytesSentInInterval,
                                final: true
                            });
                        } else {
                            uploadWorker.postMessage('progress', {
                                workerId: uploadWorker.workerId,
                                bytes: 0,
                                final: true
                            });
                        }
                        
                        controller.close();
                        return;
                    }

                    const remainingBytes = totalSize - this.sentBytes;
                    const chunkSize = Math.min(CHUNK_SIZE, remainingBytes);
                    const chunk = new Uint8Array(chunkSize);
                    
                    rng.fill_bytes(chunk);
                    this.sentBytes += chunkSize;
                    bytesSentInInterval += chunkSize;
                    
                    controller.enqueue(chunk);
                }
            });

            const uploadWorker = this;
            const response = await fetch(url.href, {
                method: 'POST',
                body: stream,
                signal: this.abortController.signal,
                headers: {
                    'Content-Type': 'application/octet-stream'
                },
                duplex: 'half' // Required for streaming uploads
            });

            clearInterval(progressTimer);

            if (!response.ok) {
                throw new Error(`HTTP ${response.status}: ${response.statusText}`);
            }

            const mbSI = (totalSize / 1000000).toFixed(2);
            const mibIEC = (totalSize / (1024 * 1024)).toFixed(2);
            this.postLog(`Upload complete: ${mbSI} MB (SI) / ${mibIEC} MiB (IEC)`);
            
            // Finally send the done message
            this.postMessage('done', { workerId: this.workerId });
            
        } catch (err) {
            if (err.name === 'AbortError') {
                this.postLog(`Upload cancelled`);
            } else {
                this.postLog(`Error: ${err.message}`, 'error');
            }
            
            this.postMessage('done', { workerId: this.workerId });
        }
    }

    async stopUpload() {
        try {
            if (this.abortController) {
                this.abortController.abort();
                this.abortController = null;
            }
            this.postLog(`Upload stopped by user`);
        } catch (err) {
            this.postLog(`Error stopping upload: ${err.message}`, 'error');
        }
    }

    randomBigInt64() {
        const arr = new Uint32Array(2);
        crypto.getRandomValues(arr);
        return (BigInt(arr[1]) << 32n) | BigInt(arr[0]);
    }

    postLog(message, className = 'info') {
        this.postMessage('log', { workerId: this.workerId, message, className });
    }

    postMessage(type, data = {}) {
        self.postMessage({ type, data });
    }
}

// Initialize worker
const worker = new FetchUploadWorker();
