package main

import (
	"archive/tar"
	"compress/gzip"
	"io"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/zR-JB/graphite-meter/go/internal/legal"
)

func TestPackageRootHandlesScopedNPMModules(t *testing.T) {
	root := t.TempDir()
	packageDir := filepath.Join(root, "node_modules", "@scope", "package")
	if err := os.MkdirAll(filepath.Join(packageDir, "dist"), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(packageDir, "package.json"), []byte(`{"name":"@scope/package","version":"1.0.0"}`), 0o644); err != nil {
		t.Fatal(err)
	}
	got, err := packageRoot(filepath.Join(packageDir, "dist", "index.js"))
	if err != nil {
		t.Fatal(err)
	}
	if got != packageDir {
		t.Fatalf("package root = %q, want %q", got, packageDir)
	}
}

func TestSourceURLUsesReleaseTagOnlyForReleaseVersions(t *testing.T) {
	if !releaseVersion.MatchString("1.2.3-rc.4") {
		t.Fatal("release expression does not accept rc versions")
	}
	if releaseVersion.MatchString("0.0.0-dev+abc") {
		t.Fatal("development version was treated as a release")
	}
}

func TestSourceBundleIsDeterministicAndIncludesManualMaterial(t *testing.T) {
	repo := t.TempDir()
	if err := os.MkdirAll(filepath.Join(repo, "project"), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(repo, "LICENSE"), []byte("project license\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	manual := filepath.Join(repo, "manual.txt")
	if err := os.WriteFile(manual, []byte("manual source\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	provenance := []legal.Provenance{{Name: "sample", LocalPaths: []string{"manual.txt"}}}
	outDir := t.TempDir()
	first := filepath.Join(outDir, "first.tar.gz")
	second := filepath.Join(outDir, "second.tar.gz")
	project := legal.Project{Name: "Graphite Meter", Repository: "https://example.invalid/repo"}
	if err := sourceBundle(repo, project, "development", nil, nil, nil, provenance, first); err != nil {
		t.Fatal(err)
	}
	if err := sourceBundle(repo, project, "development", nil, nil, nil, provenance, second); err != nil {
		t.Fatal(err)
	}
	one, err := os.ReadFile(first)
	if err != nil {
		t.Fatal(err)
	}
	two, err := os.ReadFile(second)
	if err != nil {
		t.Fatal(err)
	}
	if string(one) != string(two) {
		t.Fatal("corresponding-source archive is not deterministic")
	}
	file, err := os.Open(first)
	if err != nil {
		t.Fatal(err)
	}
	defer file.Close()
	reader, err := gzip.NewReader(file)
	if err != nil {
		t.Fatal(err)
	}
	defer reader.Close()
	tarReader := tar.NewReader(reader)
	var names []string
	for {
		header, readErr := tarReader.Next()
		if readErr == io.EOF {
			break
		}
		if readErr != nil {
			t.Fatal(readErr)
		}
		names = append(names, header.Name)
	}
	joined := strings.Join(names, "\n")
	for _, want := range []string{
		"graphite-meter_development_corresponding-source/project/LICENSE",
		"graphite-meter_development_corresponding-source/third_party/manual/sample/manual.txt",
		"graphite-meter_development_corresponding-source/LEGAL_INVENTORY.json",
		"graphite-meter_development_corresponding-source/README.txt",
	} {
		if !strings.Contains(joined, want) {
			t.Fatalf("archive does not contain %s", want)
		}
	}
}

func TestSourceBundleHelpers(t *testing.T) {
	if got := safeName("github.com/example/pkg@v1"); got != "github.com_example_pkg_at_v1" {
		t.Fatalf("safeName = %q", got)
	}
	files := legalFilePaths([]legal.LegalFile{{Name: "LICENSE"}, {Name: "NOTICE"}})
	if strings.Join(files, ",") != "LICENSE,NOTICE" {
		t.Fatalf("legalFilePaths = %v", files)
	}
	if _, err := moduleDirectory(t.TempDir(), "missing/module", "v0.0.0"); err == nil {
		t.Fatal("missing module unexpectedly resolved")
	}
}
