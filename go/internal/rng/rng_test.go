package rng

import (
	"bufio"
	"encoding/hex"
	"os"
	"strconv"
	"strings"
	"testing"
)

const corpusPath = "../../../api/rng.testvectors.txt"

// vector is one row of api/rng.testvectors.txt: seed → first-n bytes.
type vector struct {
	line      int
	seed, inc uint64
	n         int
	wantHex   string
}

// loadCorpus parses the shared byte-exact corpus, skipping comment/blank lines.
func loadCorpus(t *testing.T) []vector {
	t.Helper()
	f, err := os.Open(corpusPath)
	if err != nil {
		t.Fatalf("open corpus: %v", err)
	}
	defer f.Close()

	var vs []vector
	sc := bufio.NewScanner(f)
	ln := 0
	for sc.Scan() {
		ln++
		line := strings.TrimSpace(sc.Text())
		if line == "" || strings.HasPrefix(line, "#") {
			continue
		}
		parts := strings.Split(line, "|")
		if len(parts) != 4 {
			t.Fatalf("line %d: want 4 fields, got %d: %q", ln, len(parts), line)
		}
		seed, err := strconv.ParseUint(strings.TrimSpace(parts[0]), 10, 64)
		if err != nil {
			t.Fatalf("line %d: seed: %v", ln, err)
		}
		inc, err := strconv.ParseUint(strings.TrimSpace(parts[1]), 10, 64)
		if err != nil {
			t.Fatalf("line %d: inc: %v", ln, err)
		}
		n, err := strconv.Atoi(strings.TrimSpace(parts[2]))
		if err != nil {
			t.Fatalf("line %d: n: %v", ln, err)
		}
		vs = append(vs, vector{line: ln, seed: seed, inc: inc, n: n, wantHex: strings.TrimSpace(parts[3])})
	}
	if err := sc.Err(); err != nil {
		t.Fatalf("scan corpus: %v", err)
	}
	if len(vs) == 0 {
		t.Fatal("corpus is empty — expected populated test vectors")
	}
	return vs
}

// TestFillMatchesCorpus is the byte-exact conformance check: every row's
// seed/inc must produce its pinned first-n bytes. This is the cross-language
// contract (Surface C) a Rust/WASM port asserts against too.
func TestFillMatchesCorpus(t *testing.T) {
	for _, v := range loadCorpus(t) {
		dst := make([]byte, v.n)
		Fill(dst, v.seed, v.inc)
		if got := hex.EncodeToString(dst); got != v.wantHex {
			t.Errorf("line %d: Fill(seed=%d, inc=%d, n=%d)\n got  %s\n want %s",
				v.line, v.seed, v.inc, v.n, got, v.wantHex)
		}
	}
}

// TestFillIsPrefixConsistent guards the streaming invariant the download path
// relies on: a shorter Fill must be a byte-exact prefix of a longer one for the
// same seed/inc (so block slices line up regardless of request size).
func TestFillIsPrefixConsistent(t *testing.T) {
	const seed, inc = BlockSeed, BlockInc
	long := make([]byte, 257)
	Fill(long, seed, inc)
	for _, n := range []int{0, 1, 7, 8, 9, 64, 255, 256} {
		short := make([]byte, n)
		Fill(short, seed, inc)
		if string(short) != string(long[:n]) {
			t.Errorf("Fill n=%d is not a prefix of the longer stream", n)
		}
	}
}

// TestNewBlock checks the shared block is the right size and matches Fill from
// the canonical parameters.
func TestNewBlock(t *testing.T) {
	b := NewBlock(BlockSize)
	if len(b) != BlockSize {
		t.Fatalf("len = %d, want %d", len(b), BlockSize)
	}
	want := make([]byte, BlockSize)
	Fill(want, BlockSeed, BlockInc)
	if string(b) != string(want) {
		t.Error("NewBlock does not match Fill(BlockSeed, BlockInc)")
	}
}
