// Utility functions for the WebTransport Upload Client

/**
 * Format bits into human-readable format with auto-scaling
 * @param {number} bits - The number of bits to format
 * @param {number} decimals - Number of decimal places
 * @returns {string} Formatted string with unit
 */
export function formatBits(bits, decimals = 2) {
    if (bits === 0) return '0 bps';
    const k = 1000;
    const sizes = ['bps', 'Kbps', 'Mbps', 'Gbps', 'Tbps'];
    const i = Math.floor(Math.log(bits) / Math.log(k));
    return `${(bits / Math.pow(k, i)).toFixed(decimals)} ${sizes[i]}`;
}

/**
 * Format bytes into human-readable format
 * @param {number} bytes - The number of bytes to format
 * @param {Object} options - Formatting options
 * @param {number} options.base - The base for conversion (1000 for SI, 1024 for IEC)
 * @param {number} options.decimals - Number of decimal places
 * @param {boolean} options.iec - Whether to use IEC units (KiB, MiB) or SI units (KB, MB)
 * @returns {string} Formatted string with unit
 */
export function formatBytes(bytes, { base = 1000, decimals = 2, iec = false } = {}) {
    if (bytes === 0) return '0 B';
    const k = base;
    const sizes = iec
        ? ['B', 'KiB', 'MiB', 'GiB', 'TiB']
        : ['B', 'KB', 'MB', 'GB', 'TB'];
    const i = Math.floor(Math.log(bytes) / Math.log(k));
    return `${(bytes / Math.pow(k, i)).toFixed(decimals)} ${sizes[i]}`;
}

/**
 * Format bits per second into human-readable format
 * @param {number} bps - Bits per second
 * @param {number} decimals - Number of decimal places
 * @returns {Object} Object with formatted strings for different units
 */
export function formatBitrate(bps, decimals = 2) {
    return {
        bps: bps.toFixed(decimals),
        kbps: (bps / 1000).toFixed(decimals),
        Mbps: (bps / 1e6).toFixed(decimals),
        Gbps: (bps / 1e9).toFixed(decimals)
    };
}

// Export state management from state.js for easier imports
export { AppState, StateTransitions, StateManager, createStateMessage } from './state.js';