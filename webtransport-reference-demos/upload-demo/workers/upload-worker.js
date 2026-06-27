import init, { ScrambledCounterRng } from './pkg/rngwasm.js';
import { AppState } from '../utils.js';

class UploadWorker {
  constructor() {
    this.transport = null;
    this.currentState = AppState.INIT;
    this.workerId = null;

    this.config = {
      url: '',
      sizeBytes: 0,
      measurementMode: 'client', // 'client' or 'server'
    };

    this.stats = {
      bytesSent: 0,
      startTime: null,
    };

    this.stopRequested = false;
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
        this.config.measurementMode = data.measurementMode || 'client';
        this.startUpload();
      } else if (type === 'stop') {
        this.stopUpload();
      }
    };
  }

  handleStateChange(newState) {
    this.currentState = newState;
  }

  
async startUpload() {
  try {
    this.postLog(
      `Connecting to ${this.config.url} (mode: ${this.config.measurementMode})`
    );

    // Create WebTransport connection
    this.transport = new WebTransport(this.config.url, {
      allowPooling: false,
      requireUnreliable: true,
      congestionControl: "throughput",
    });

    await this.transport.ready;

    if (this.config.measurementMode === "server") {
      this.startDatagramReader();
    }

    // Init RNG once
    await init();
    const rng = new ScrambledCounterRng(
      this.randomBigInt64(),
      this.randomBigInt64()
    );

    const totalSize = this.config.sizeBytes;



    // Configurable number of parallel streams
    const STREAM_COUNT = 4; // Change as needed or make configurable
    const CHUNK_SIZE = 2 << 24; // 16 MiB (2^24 bytes)
    this.stopRequested = false;

    // Divide total size among streams
    const sizePerStream = Math.floor(totalSize / STREAM_COUNT);
    let bytesSent = 0;

    // Helper for each stream
    const sendStreamTask = async (streamIdx) => {
      const sendStream = await this.transport.createUnidirectionalStream();
      const writer = sendStream.getWriter();
      const buffer = new Uint8Array(CHUNK_SIZE);
      let sent = 0;
      while (!this.stopRequested && sent < sizePerStream) {
        const remaining = sizePerStream - sent;
        const size = Math.min(CHUNK_SIZE, remaining);
        rng.fill_bytes(buffer.subarray(0, size));
        sent += size;
        bytesSent += size;

        if (this.config.measurementMode === "client") {
          this.postMessage("progress", {
            workerId: this.workerId,
            bytes: size,
            final: bytesSent >= totalSize,
          });
        }
        await writer.write(buffer.subarray(0, size));
      }
      await writer.close();
    };

    // Start all streams in parallel
    await Promise.all(Array.from({ length: STREAM_COUNT }, (_, i) => sendStreamTask(i)));

    this.postLog(
      `Upload complete: ${(totalSize / (1024 * 1024)).toFixed(2)} MiB sent`
    );

    if (this.config.measurementMode === "server") {
      this.postLog("Waiting for final server measurements...");
      await new Promise((r) => setTimeout(r, 1000));
    }

    await this.transport.close();
    this.postMessage("done", { workerId: this.workerId });

  } catch (err) {
    this.postLog(`Error: ${err.message}`, "error");
    this.postMessage("done", { workerId: this.workerId });
  }
}


    startDatagramReader() {
    (async () => {
        try {
        const reader = this.transport.datagrams.readable.getReader();
        while (true) {
            const { value, done } = await reader.read();
            if (done) break;
            const msg = new TextDecoder().decode(value);

            if (msg.startsWith('BYTES_RECEIVED,')) {
            const bytes = parseInt(msg.substring(15), 10);
            this.postMessage('server_progress', {
                workerId: this.workerId,
                bytes,
            });
            } 
            else if (msg.startsWith('UPLOAD_COMPLETE,')) {
            const bytes = parseInt(msg.substring(16), 10);
            this.postMessage('server_progress', {
                workerId: this.workerId,
                bytes,
                final: true,
            });
            this.postLog(`Server reported upload complete: ${bytes} bytes`, 'success');

            // Stop reading datagrams immediately
            try { await reader.cancel(); } catch {}
            break;
            }
        }
        } catch (err) {
        this.postLog(`Datagram reader error: ${err.message}`, 'error');
        }
    })();
    }

  async stopUpload() {
    this.stopRequested = true;
    try {
      if (this.transport) {
        await this.transport.close();
        this.transport = null;
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
const worker = new UploadWorker();