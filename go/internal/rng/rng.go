// Package rng is the canonical scrambled-counter xorshift64* generator.
// It produces incompressible bytes for the download path and maintains
// byte-for-byte identity across the server's Go implementation and any future port.
//
// Generator:
//
//	next:  old = state; state = old + inc; return scramble(old)
//	scramble(x): x ^= x>>12; x ^= x<<25; x ^= x>>27; return x * 0x2545F4914F6CDD1D
//
// fill writes successive next() as little-endian u64; a tail < 8 bytes is the
// low bytes of one more next() in little-endian order.
package rng

import "encoding/binary"

// scrambleMul is the xorshift64* output multiplier.
const scrambleMul = 0x2545F4914F6CDD1D

// Canonical download-block parameters. Arbitrary-but-fixed: BlockSeed is the
// 64-bit golden-ratio constant and BlockInc is an odd increment (so the additive
// counter has full period). These MUST match any future port.
const (
	BlockSeed uint64 = 0x9E3779B97F4A7C15
	BlockInc  uint64 = 0x2545F4914F6CDD1D
	// BlockSize is the shared immutable download block size (256 KiB — large
	// enough to defeat transport compression while staying L2-cache friendly).
	BlockSize = 256 * 1024
)

// Source is a deterministic xorshift64* stream: a 64-bit additive counter whose
// pre-increment state is scrambled into each output word. Not safe for
// concurrent use — generate a block once at startup and read from it instead.
type Source struct {
	state uint64
	inc   uint64
}

// New returns a Source seeded with the given initial state and increment.
func New(seed, inc uint64) *Source { return &Source{state: seed, inc: inc} }

// Next returns the next 64-bit output word and advances the counter.
func (s *Source) Next() uint64 {
	old := s.state
	s.state = old + s.inc
	return scramble(old)
}

// Fill writes dst per the little-endian algorithm described in the package doc.
func (s *Source) Fill(dst []byte) {
	var word [8]byte
	i := 0
	for ; i+8 <= len(dst); i += 8 {
		binary.LittleEndian.PutUint64(dst[i:], s.Next())
	}
	if i < len(dst) {
		binary.LittleEndian.PutUint64(word[:], s.Next())
		copy(dst[i:], word[:])
	}
}

// Fill is the package-level convenience: fill dst from a fresh Source(seed, inc).
func Fill(dst []byte, seed, inc uint64) { New(seed, inc).Fill(dst) }

// NewBlock returns a freshly generated immutable block of size bytes, filled from
// the canonical download-block parameters. Generated once at startup; the
// download endpoint serves slices of it without ever regenerating.
func NewBlock(size int) []byte {
	b := make([]byte, size)
	Fill(b, BlockSeed, BlockInc)
	return b
}

// scramble applies the xorshift64* mix to a counter word.
func scramble(x uint64) uint64 {
	x ^= x >> 12
	x ^= x << 25
	x ^= x >> 27
	return x * scrambleMul
}
