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

func TestSourceBundleIncludesBrowserComponent(t *testing.T) {
	repo := t.TempDir()
	packageDir := filepath.Join(repo, "client", "node_modules", "svelte")
	if err := os.MkdirAll(packageDir, 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(packageDir, "package.json"), []byte(`{"name":"svelte","version":"5.56.8"}`), 0o644); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(packageDir, "LICENSE.md"), []byte("MIT\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	out := filepath.Join(t.TempDir(), "source.tar.gz")
	component := legal.Component{Name: "svelte", Version: "5.56.8", Ecosystem: "npm"}
	if err := sourceBundle(repo, legal.Project{}, "development", []legal.Component{component}, nil, nil, nil, out); err != nil {
		t.Fatal(err)
	}
	file, err := os.Open(out)
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
	found := false
	for {
		header, readErr := tarReader.Next()
		if readErr == io.EOF {
			break
		}
		if readErr != nil {
			t.Fatal(readErr)
		}
		if strings.HasSuffix(header.Name, "/third_party/npm/svelte_at_5.56.8/LICENSE.md") {
			found = true
		}
	}
	if !found {
		t.Fatal("browser source component was not archived")
	}
}

func TestLegalGeneratorFormattingHelpers(t *testing.T) {
	if sourceFor("github.com/example/module", "") != "https://github.com/example/module" {
		t.Fatal("github source URL mismatch")
	}
	if sourceFor("golang.org/x/net", "") != "https://go.googlesource.com/net" {
		t.Fatal("Go source URL mismatch")
	}
	if sourceFor("example.invalid/module", "https://upstream.invalid") != "https://upstream.invalid" {
		t.Fatal("explicit source URL mismatch")
	}
	if packageLicense("MIT", nil, "UNKNOWN") != "MIT" {
		t.Fatal("string package license mismatch")
	}
	if packageLicense(nil, []struct {
		Type string `json:"type"`
	}{{Type: "ISC"}}, "UNKNOWN") != "ISC" {
		t.Fatal("package license list mismatch")
	}
	if repositoryURL(map[string]any{"url": "git+https://github.com/example/module.git"}, "x") != "https://github.com/example/module" {
		t.Fatal("repository URL normalization mismatch")
	}
	if inferLicense([]legal.LegalFile{{Text: "MIT License\n"}}) != "MIT" {
		t.Fatal("license inference mismatch")
	}
	components := sortComponents([]legal.Component{
		{Name: "b", Version: "1", Ecosystem: "go"},
		{Name: "a", Version: "1", Ecosystem: "go"},
		{Name: "a", Version: "1", Ecosystem: "go", Source: "same"},
	})
	if len(components) != 2 || components[0].Name != "a" || components[1].Name != "b" {
		t.Fatalf("component ordering/deduplication mismatch: %#v", components)
	}
	if yesNo(true) != "yes" || yesNo(false) != "no" || !contains([]string{"tui"}, "tui") {
		t.Fatal("formatting helper mismatch")
	}
	notice := notices([]legal.Component{{
		Name: "demo", Version: "1", Source: "https://example.invalid",
		SelectedLicenseExpression: "MIT",
		LegalTexts:                []legal.LegalFile{{Name: "LICENSE", Text: "text"}},
	}})
	if !strings.Contains(notice, "Component: demo") || !strings.Contains(notice, "--- LICENSE ---") {
		t.Fatal("notice rendering mismatch")
	}
	if data, err := marshal(map[string]string{"ok": "yes"}); err != nil || !strings.Contains(string(data), "ok") {
		t.Fatal("JSON rendering mismatch")
	}
}

func TestLicenseInferenceAndApacheNoticeSeparation(t *testing.T) {
	for _, test := range []struct {
		name, text, want string
	}{
		{"MIT", "MIT License\nPermission is hereby granted, free of charge", "MIT"},
		{"ISC", "Permission to use, copy, modify, and distribute", "ISC"},
		{"BSD", "BSD 3-Clause\nRedistribution and use in source and binary forms", "BSD-3-Clause"},
		{"Apache", "                                  Apache License\n                              Version 2.0", "Apache-2.0"},
	} {
		t.Run(test.name, func(t *testing.T) {
			component := componentFromFiles("go", "example", "v1", "https://example.invalid", []legal.LegalFile{{Name: "LICENSE", Text: test.text, Kind: "license"}})
			if component.SelectedLicenseExpression != test.want {
				t.Fatalf("license = %q, want %q", component.SelectedLicenseExpression, test.want)
			}
		})
	}
	component := componentFromFiles("go", "apache", "v1", "https://example.invalid", []legal.LegalFile{
		{Name: "LICENSE", Text: "Apache License Version 2.0", Kind: "license"},
		{Name: "NOTICE", Text: "Upstream notice", Kind: "notice"},
	})
	if len(component.LegalTexts) != 1 || len(component.Notices) != 1 {
		t.Fatalf("Apache legal/notice split = %#v / %#v", component.LegalTexts, component.Notices)
	}
}

func TestGoReplacementRequiresProvenance(t *testing.T) {
	if hasGoReplacementProvenance(nil, "example/replacement", "v1.2.3", "server") {
		t.Fatal("unrecorded replacement was accepted")
	}
	entries := []legal.Provenance{{Ecosystem: "go", Name: "example/replacement", Version: "v1.2.3", ArtifactScopes: []string{"server/browser"}}}
	if !hasGoReplacementProvenance(entries, "example/replacement", "v1.2.3", "server") {
		t.Fatal("matching replacement provenance was rejected")
	}
	if !hasGoReplacementProvenance(entries, "example/replacement", "", "server") {
		t.Fatal("local replacement without module version was rejected")
	}
	if hasGoReplacementProvenance(entries, "example/replacement", "v1.2.3", "tui") {
		t.Fatal("out-of-scope replacement provenance was accepted")
	}
}

func TestSourceBundleExcludesGeneratedCoverageProfile(t *testing.T) {
	repo := t.TempDir()
	if err := os.MkdirAll(filepath.Join(repo, "go"), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(repo, "go", "cover.out"), []byte("generated profile\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	out := filepath.Join(t.TempDir(), "source.tar.gz")
	if err := sourceBundle(repo, legal.Project{}, "development", nil, nil, nil, nil, out); err != nil {
		t.Fatal(err)
	}
	file, err := os.Open(out)
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
	for {
		header, readErr := tarReader.Next()
		if readErr == io.EOF {
			return
		}
		if readErr != nil {
			t.Fatal(readErr)
		}
		if strings.Contains(header.Name, "cover.out") {
			t.Fatalf("generated coverage profile leaked into source archive: %s", header.Name)
		}
	}
}
