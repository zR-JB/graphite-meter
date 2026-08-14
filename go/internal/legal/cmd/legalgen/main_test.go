package main

import (
	"os"
	"path/filepath"
	"testing"
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
