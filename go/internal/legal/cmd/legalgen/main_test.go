package main

import (
	"archive/tar"
	"compress/gzip"
	"errors"
	"io"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"testing"

	"github.com/zR-JB/graphite-meter/go/internal/legal"
)

type tarGzArchive struct {
	names    []string
	contents map[string]string
}

func readTarGz(t *testing.T, path string) tarGzArchive {
	t.Helper()
	file, err := os.Open(path)
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
	archive := tarGzArchive{contents: make(map[string]string)}
	for {
		header, readErr := tarReader.Next()
		if errors.Is(readErr, io.EOF) {
			return archive
		}
		if readErr != nil {
			t.Fatal(readErr)
		}
		archive.names = append(archive.names, header.Name)
		data, err := io.ReadAll(tarReader)
		if err != nil {
			t.Fatal(err)
		}
		archive.contents[header.Name] = string(data)
	}
}

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

func TestLegalGoVersionNormalizesToolchainSuffixes(t *testing.T) {
	for _, test := range []struct {
		raw, want string
	}{
		{"go1.26.6", "go1.26.6"},
		{"go1.26.6-X:nodwarf5", "go1.26.6"},
		{"go1.26.6 local build", "go1.26.6"},
		{"devel go1.27-abcdef", "devel go1.27-abcdef"},
	} {
		t.Run(test.raw, func(t *testing.T) {
			if got := legalGoVersion(test.raw); got != test.want {
				t.Fatalf("legalGoVersion(%q) = %q, want %q", test.raw, got, test.want)
			}
		})
	}
}

func TestRepositoryGoToolchainVersionUsesExactPin(t *testing.T) {
	repo := t.TempDir()
	goDir := filepath.Join(repo, "go")
	if err := os.MkdirAll(goDir, 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(
		filepath.Join(goDir, "go.mod"),
		[]byte("module example.invalid/test\n\ngo 1.27.0\n"),
		0o644,
	); err != nil {
		t.Fatal(err)
	}
	got, err := repositoryGoToolchainVersion(repo)
	if err != nil {
		t.Fatal(err)
	}
	if got != "go1.27.0" {
		t.Fatalf("repositoryGoToolchainVersion = %q, want go1.27.0", got)
	}
}

func TestRepositoryGoToolchainVersionRequiresExactPin(t *testing.T) {
	for _, contents := range []string{
		"module example.invalid/test\n\ngo 1.27\n",
		"module example.invalid/test\n\ngo latest\n",
		"module example.invalid/test\n\ngo 1.27.0\ntoolchain go1.27.0 extra\n",
	} {
		repo := t.TempDir()
		goDir := filepath.Join(repo, "go")
		if err := os.MkdirAll(goDir, 0o755); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(filepath.Join(goDir, "go.mod"), []byte(contents), 0o644); err != nil {
			t.Fatal(err)
		}
		if _, err := repositoryGoToolchainVersion(repo); err == nil {
			t.Fatalf("repositoryGoToolchainVersion unexpectedly accepted %q", contents)
		}
	}
}

func TestValidateGoToolchainVersionExplainsMismatch(t *testing.T) {
	err := validateGoToolchainVersion("go1.26.6", "go1.26.5")
	if err == nil || !strings.Contains(err.Error(), "go/go.mod pins go1.26.6 but legalgen is running go1.26.5") {
		t.Fatalf("unexpected mismatch error: %v", err)
	}
}

func TestGoToolchainComponentIgnoresGOROOTFormatting(t *testing.T) {
	repo := t.TempDir()
	goDir := filepath.Join(repo, "go")
	toolchainDir := filepath.Join(repo, "legal", "toolchains", "go")
	if err := os.MkdirAll(goDir, 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.MkdirAll(toolchainDir, 0o755); err != nil {
		t.Fatal(err)
	}
	version := legalGoVersion(runtime.Version())
	if err := os.WriteFile(
		filepath.Join(goDir, "go.mod"),
		[]byte("module example.invalid/test\n\ngo 1.26.5\ntoolchain "+version+"\n"),
		0o644,
	); err != nil {
		t.Fatal(err)
	}
	fakeGOROOT := t.TempDir()
	if err := os.WriteFile(filepath.Join(fakeGOROOT, "PATENTS"), []byte("local GOROOT formatting\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	t.Setenv("GOROOT", fakeGOROOT)
	if err := os.WriteFile(filepath.Join(toolchainDir, "LICENSE"), []byte("Redistribution and use in source and binary forms\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(toolchainDir, "PATENTS"), []byte("canonical patents snapshot\n"), 0o644); err != nil {
		t.Fatal(err)
	}

	component, err := goToolchainComponent(repo)
	if err != nil {
		t.Fatal(err)
	}
	if len(component.LegalTexts) != 1 || component.LegalTexts[0].Name != "LICENSE" {
		t.Fatalf("canonical legal files = %#v", component.LegalTexts)
	}
	if len(component.Notices) != 1 || component.Notices[0].Name != "PATENTS" || component.Notices[0].Text != "canonical patents snapshot\n" {
		t.Fatalf("canonical notices = %#v", component.Notices)
	}
}

func TestThirdPartySourceBundleIsDeterministicAndExcludesProjectSource(t *testing.T) {
	repo := t.TempDir()
	if err := os.WriteFile(filepath.Join(repo, "LICENSE"), []byte("project license\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	manual := filepath.Join(repo, "manual.txt")
	if err := os.WriteFile(manual, []byte("manual source\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	certDir := filepath.Join(repo, ".dev-certs")
	if err := os.MkdirAll(certDir, 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(certDir, "development.pem"), []byte("private material\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	provenance := []legal.Provenance{{
		Name:                "sample",
		LocalPaths:          []string{"manual.txt"},
		CorrespondingSource: "third_party/manual/sample",
	}}
	outDir := t.TempDir()
	first := filepath.Join(outDir, "first.tar.gz")
	second := filepath.Join(outDir, "second.tar.gz")
	project := legal.Project{Name: "Graphite Meter", Repository: "https://example.invalid/repo"}
	if err := thirdPartySourceBundle(repo, project, "development", nil, nil, nil, provenance, first); err != nil {
		t.Fatal(err)
	}
	if err := thirdPartySourceBundle(repo, project, "development", nil, nil, nil, provenance, second); err != nil {
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
		t.Fatal("third-party source archive is not deterministic")
	}
	archive := readTarGz(t, first)
	joined := strings.Join(archive.names, "\n")
	if strings.Contains(joined, "/project/") || strings.Contains(joined, "/LICENSE") {
		t.Fatal("project source was duplicated into third-party source archive")
	}
	if strings.Contains(joined, ".dev-certs") {
		t.Fatal("developer-local project material leaked into third-party source archive")
	}
	root := "graphite-meter_development_third-party-source"
	for _, want := range []string{
		root + "/third_party/manual/sample/manual.txt",
		root + "/LEGAL_INVENTORY.json",
		root + "/PROVENANCE.json",
		root + "/README.txt",
	} {
		if !strings.Contains(joined, want) {
			t.Fatalf("archive does not contain %s", want)
		}
	}
	readme := archive.contents[root+"/README.txt"]
	if !strings.Contains(readme, "Source code (tar.gz)") || !strings.Contains(readme, project.Repository) {
		t.Fatalf("third-party source README does not explain the split source offer: %q", readme)
	}
}

func TestThirdPartySourceBundleHelpers(t *testing.T) {
	if got := safeName("github.com/example/pkg@v1"); got != "github.com_example_pkg_at_v1" {
		t.Fatalf("safeName = %q", got)
	}
	if _, err := moduleDirectory(t.TempDir(), "missing/module", "v0.0.0"); err == nil {
		t.Fatal("missing module unexpectedly resolved")
	}
	got, err := manualSourceDestination(legal.Provenance{Name: "sample", CorrespondingSource: "third_party/manual/sample"})
	if err != nil || got != "third_party/manual/sample" {
		t.Fatalf("manualSourceDestination = %q, %v", got, err)
	}
	for _, bad := range []string{"../escape", "third_party/go/not-manual", "/absolute", `third_party\\manual\\windows`} {
		if _, err := manualSourceDestination(legal.Provenance{Name: "sample", CorrespondingSource: bad}); err == nil {
			t.Fatalf("manualSourceDestination unexpectedly accepted %q", bad)
		}
	}
}

func TestThirdPartySourceBundleIncludesBrowserComponent(t *testing.T) {
	repo := t.TempDir()
	packageDir := filepath.Join(repo, "client", "node_modules", "outer", "node_modules", "svelte")
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
	component := legal.Component{Name: "svelte", Version: "5.56.8", Ecosystem: "npm", SourcePath: packageDir}
	if err := thirdPartySourceBundle(repo, legal.Project{}, "development", []legal.Component{component}, nil, nil, nil, out); err != nil {
		t.Fatal(err)
	}
	archive := readTarGz(t, out)
	found := false
	for _, name := range archive.names {
		if strings.HasSuffix(name, "/third_party/npm/svelte_at_5.56.8/LICENSE.md") {
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

func TestReviewedLegalFilesRehashesCurrentBytes(t *testing.T) {
	dir := t.TempDir()
	current := []byte("current legal text")
	if err := os.WriteFile(filepath.Join(dir, "LICENSE"), current, 0o644); err != nil {
		t.Fatal(err)
	}
	reviews := []legal.Review{{
		Name: "example", Ecosystem: "go",
		LegalFiles: []legal.LegalFile{{Name: "LICENSE", SHA256: legal.SHA256([]byte("approved text"))}},
	}}
	files, err := reviewedLegalFiles(dir, "go", "example", reviews)
	if err != nil {
		t.Fatal(err)
	}
	if files[0].SHA256 != legal.SHA256(current) {
		t.Fatalf("current hash = %q, want %q", files[0].SHA256, legal.SHA256(current))
	}
}

func TestReviewedLegalFilesResolveBunIsolatedDependencySibling(t *testing.T) {
	store := t.TempDir()
	packageDir := filepath.Join(store, "svelte")
	dependencyLicense := filepath.Join(store, "magic-string", "LICENSE")
	if err := os.MkdirAll(packageDir, 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.MkdirAll(filepath.Dir(dependencyLicense), 0o755); err != nil {
		t.Fatal(err)
	}
	current := []byte("isolated dependency license")
	if err := os.WriteFile(dependencyLicense, current, 0o644); err != nil {
		t.Fatal(err)
	}
	reviews := []legal.Review{{
		Name: "svelte", Ecosystem: "npm",
		LegalFiles: []legal.LegalFile{{Name: "node_modules/magic-string/LICENSE"}},
	}}
	files, err := reviewedLegalFiles(packageDir, "npm", "svelte", reviews)
	if err != nil {
		t.Fatal(err)
	}
	if files[0].Name != "node_modules/magic-string/LICENSE" || files[0].Text != string(current) {
		t.Fatalf("isolated legal file = %#v", files[0])
	}
}

func TestReviewedSelectionBecomesAuthoritativeAfterValidation(t *testing.T) {
	scopes := []componentScope{{name: "tui", components: []legal.Component{{
		Name: "example", Ecosystem: "npm", SelectedLicenseExpression: "MIT OR GPL-3.0",
	}}}}
	reviews := []legal.Review{{
		Name: "example", Ecosystem: "npm", SelectedLicenseExpression: "MIT",
	}}
	if err := prepareScopes(scopes, reviews, "review-template"); err != nil {
		t.Fatal(err)
	}
	if scopes[0].components[0].SelectedLicenseExpression != "MIT" {
		t.Fatalf("selected license = %q, want reviewed MIT", scopes[0].components[0].SelectedLicenseExpression)
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

func TestGoDiscoveryTargetsIncludeServerArchitectures(t *testing.T) {
	repo, err := repositoryRoot("")
	if err != nil {
		t.Fatal(err)
	}
	var server []string
	for _, target := range mustGoDiscoveryTargets(t, repo) {
		if target.name == "server" {
			server = append(server, target.goos+"/"+target.goarch)
		}
	}
	if strings.Join(server, ",") != "linux/amd64,linux/arm64" {
		t.Fatalf("server discovery targets = %v", server)
	}
}

func TestGoDiscoveryTargetsUseCanonicalTUIList(t *testing.T) {
	repo, err := repositoryRoot("")
	if err != nil {
		t.Fatal(err)
	}
	want := []string{"linux/amd64", "linux/arm64", "darwin/amd64", "darwin/arm64", "windows/amd64"}
	var tui []string
	for _, target := range mustGoDiscoveryTargets(t, repo) {
		if target.name == "tui" {
			tui = append(tui, target.goos+"/"+target.goarch)
		}
	}
	if strings.Join(tui, ",") != strings.Join(want, ",") {
		t.Fatalf("TUI discovery targets = %v, want %v", tui, want)
	}
}

func mustGoDiscoveryTargets(t *testing.T, repo string) []goTarget {
	t.Helper()
	targets, err := goDiscoveryTargets(repo)
	if err != nil {
		t.Fatal(err)
	}
	return targets
}

func TestProvenanceHashesLocalArtifacts(t *testing.T) {
	repo := t.TempDir()
	assetPath := filepath.Join(repo, "font.woff2")
	if err := os.WriteFile(assetPath, []byte("approved font bytes\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	entry := legal.Provenance{
		Ecosystem: "font", Name: "Example Font", Version: "1", LicenseExpression: "OFL-1.1",
		ArtifactScopes: []string{"server/browser"}, ReviewNotes: "reviewed",
		LocalArtifacts: []legal.LocalArtifact{{Path: "font.woff2", SHA256: legal.SHA256([]byte("approved font bytes\n"))}},
	}
	if _, err := addProvenance(repo, nil, []legal.Provenance{entry}, "server/browser"); err != nil {
		t.Fatalf("unchanged local artifact rejected: %v", err)
	}
	if err := os.WriteFile(assetPath, []byte("changed font bytes\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	if _, err := addProvenance(repo, nil, []legal.Provenance{entry}, "server/browser"); err == nil || !strings.Contains(err.Error(), "local artifact hash changed") {
		t.Fatalf("changed local artifact was accepted: %v", err)
	}
}

func TestServerBrowserProvenanceFlowsIntoContainer(t *testing.T) {
	repo := t.TempDir()
	legalPath := filepath.Join(repo, "manual", "LICENSE")
	text := []byte("MIT License\n")
	if err := os.MkdirAll(filepath.Dir(legalPath), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(legalPath, text, 0o644); err != nil {
		t.Fatal(err)
	}
	entry := legal.Provenance{
		Ecosystem: "font", Name: "Example Font", Version: "1", Upstream: "https://example.invalid/font",
		LicenseExpression: "MIT", ArtifactScopes: []string{"server/browser"}, ReviewNotes: "reviewed",
		LocalLegalFiles:     []legal.LegalFile{{Name: "manual/LICENSE", SHA256: legal.SHA256(text)}},
		CorrespondingSource: "third_party/manual/example-font",
	}
	server := mustAddProvenance(t, repo, nil, []legal.Provenance{entry}, "server/browser")
	container := mustAddProvenance(t, repo, append([]legal.Component(nil), server...), []legal.Provenance{entry}, "container")
	if len(container) != 1 || container[0].Name != entry.Name {
		t.Fatalf("container lost server/browser provenance: %#v", container)
	}
	archivePath := filepath.Join(t.TempDir(), "source.tar.gz")
	if err := thirdPartySourceBundle(repo, legal.Project{}, "development", server, nil, container, []legal.Provenance{entry}, archivePath); err != nil {
		t.Fatal(err)
	}
	archive := readTarGz(t, archivePath)
	if !strings.Contains(archive.contents["graphite-meter_development_third-party-source/LEGAL_INVENTORY.json"], entry.Name) {
		t.Fatal("container archive inventory omitted server/browser provenance")
	}
}

func mustAddProvenance(t *testing.T, repo string, components []legal.Component, entries []legal.Provenance, scope string) []legal.Component {
	t.Helper()
	result, err := addProvenance(repo, components, entries, scope)
	if err != nil {
		t.Fatal(err)
	}
	return result
}

func TestReviewedNestedLegalFilesAreRehashedAndAuthoritative(t *testing.T) {
	dir := t.TempDir()
	nested := filepath.Join(dir, "vendor", "foo", "LICENSE")
	if err := os.MkdirAll(filepath.Dir(nested), 0o755); err != nil {
		t.Fatal(err)
	}
	rootText := []byte("MIT License\nroot license\n")
	nestedText := []byte("MIT License\nnested license\n")
	if err := os.WriteFile(filepath.Join(dir, "LICENSE"), rootText, 0o644); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(nested, nestedText, 0o644); err != nil {
		t.Fatal(err)
	}
	review := legal.Review{
		Ecosystem: "npm", Name: "example", ReviewedVersion: "1.0.0",
		DeclaredLicenseExpression: "MIT", SelectedLicenseExpression: "MIT", ReviewDecision: "approved",
		LegalFiles: []legal.LegalFile{
			{Name: "LICENSE", SHA256: legal.SHA256(rootText)},
			{Name: "vendor/foo/LICENSE", SHA256: legal.SHA256(nestedText)},
		},
	}
	files, err := componentLegalFiles(dir, "npm", "example", []legal.Review{review})
	if err != nil {
		t.Fatal(err)
	}
	component := componentFromFiles("npm", "example", "1.0.0", "https://example.invalid", files)
	if err := legal.ValidateReview(component, []legal.Review{review}); err != nil {
		t.Fatalf("unchanged nested legal file rejected: %v", err)
	}
	if err := os.WriteFile(nested, []byte("changed nested license\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	files, err = componentLegalFiles(dir, "npm", "example", []legal.Review{review})
	if err != nil {
		t.Fatal(err)
	}
	component = componentFromFiles("npm", "example", "1.0.0", "https://example.invalid", files)
	if err := legal.ValidateReview(component, []legal.Review{review}); err == nil || !strings.Contains(err.Error(), "fingerprint") {
		t.Fatalf("changed nested legal file was accepted: %v", err)
	}
}

func TestThirdPartySourceBundleExcludesProjectBuildArtifacts(t *testing.T) {
	repo := t.TempDir()
	if err := os.MkdirAll(filepath.Join(repo, "go"), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(repo, "go", "cover.out"), []byte("generated profile\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	out := filepath.Join(t.TempDir(), "source.tar.gz")
	if err := thirdPartySourceBundle(repo, legal.Project{}, "development", nil, nil, nil, nil, out); err != nil {
		t.Fatal(err)
	}
	archive := readTarGz(t, out)
	for _, name := range archive.names {
		if strings.Contains(name, "cover.out") {
			t.Fatalf("project build artifact leaked into third-party source archive: %s", name)
		}
	}
}
