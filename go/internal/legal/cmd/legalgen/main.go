package main

import (
	"archive/tar"
	"bytes"
	"cmp"
	"compress/gzip"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"io"
	"maps"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"runtime"
	"slices"
	"strings"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/legal"
)

type outputFile struct {
	path string
	data []byte
}

type inventory struct {
	SchemaVersion int               `json:"schemaVersion"`
	Scope         string            `json:"scope"`
	Components    []legal.Component `json:"components"`
}

type aboutComponent struct {
	Name                      string `json:"name"`
	Version                   string `json:"version"`
	Ecosystem                 string `json:"ecosystem"`
	Source                    string `json:"source"`
	DeclaredLicenseExpression string `json:"declaredLicenseExpression"`
	SelectedLicenseExpression string `json:"selectedLicenseExpression"`
	Modified                  bool   `json:"modified"`
}

type about struct {
	SchemaVersion int              `json:"schemaVersion"`
	Project       legal.Project    `json:"project"`
	SourceVersion string           `json:"sourceVersion"`
	SourceURL     string           `json:"sourceURL"`
	LicenseURL    string           `json:"licenseURL"`
	NoticesURL    string           `json:"noticesURL"`
	Components    []aboutComponent `json:"components"`
}

type goTarget struct {
	name, goos, goarch string
}

var releaseVersion = regexp.MustCompile(`^(?:v)?[0-9]+\.[0-9]+\.[0-9]+(?:-(?:alpha|beta|rc)\.[0-9]+)?$`)
var goReleaseVersion = regexp.MustCompile(`^(go[0-9]+\.[0-9]+(?:\.[0-9]+)?)(?:[- \t].*)?$`)
var goDirectiveVersion = regexp.MustCompile(`^\d+\.\d+\.\d+$`)

func main() {
	mode := flag.String(
		"mode",
		"check",
		"check, generate, review-template, review-audit, or third-party-source-bundle",
	)
	repoFlag := flag.String("repo", "", "repository root")
	browserScan := flag.String("browser-scan", "", "JSON file emitted by the production browser scan")
	version := flag.String("version", os.Getenv("VERSION"), "release version")
	out := flag.String("out", os.Getenv("LEGAL_THIRD_PARTY_SOURCE_OUT"), "third-party source archive output path")
	flag.Parse()

	repo, err := repositoryRoot(*repoFlag)
	if err != nil {
		fatal(err)
	}
	if *version == "" {
		*version = "development"
	}

	project, err := legal.ReadProject(repo)
	if err != nil {
		fatal(err)
	}
	reviews, err := legal.ReadReviews(repo)
	if err != nil {
		fatal(err)
	}
	provenance, err := legal.ReadProvenance(repo)
	if err != nil {
		fatal(err)
	}
	serverGo, tuiGo, err := discoverGo(repo, reviews, provenance)
	if err != nil {
		fatal(err)
	}
	if *browserScan == "" {
		*browserScan = os.Getenv("GM_LEGAL_SCAN_MODULES")
	}
	if *browserScan == "" {
		fatal(errors.New("browser scan output is required; run the temporary production Vite scan first"))
	}
	browser, err := discoverBrowser(repo, *browserScan, reviews)
	if err != nil {
		fatal(err)
	}
	server := append(append([]legal.Component{}, serverGo...), browser...)
	tui := append([]legal.Component{}, tuiGo...)
	server, err = addProvenance(repo, server, provenance, "server/browser")
	if err != nil {
		fatal(err)
	}
	tui, err = addProvenance(repo, tui, provenance, "tui")
	if err != nil {
		fatal(err)
	}
	container := append([]legal.Component{}, server...)
	container, err = addProvenance(repo, container, provenance, "container")
	if err != nil {
		fatal(err)
	}

	for _, set := range []struct {
		name       string
		components []legal.Component
	}{
		{"server/browser", server}, {"tui", tui}, {"container", container},
	} {
		for _, c := range set.components {
			if err := legal.ValidateReview(c, reviews); err != nil {
				if *mode == "review-template" || *mode == "review-audit" {
					continue
				}
				fatal(fmt.Errorf("%s: %w", set.name, err))
			}
		}
	}
	server = applyReviewedSelections(server, reviews)
	tui = applyReviewedSelections(tui, reviews)
	container = applyReviewedSelections(container, reviews)
	if *mode == "review-template" {
		printReviewTemplate(server, tui, container, reviews)
		return
	}
	if *mode == "review-audit" {
		printReviewAudit(server, tui, container, reviews)
		return
	}
	if *mode == "third-party-source-bundle" {
		if err := thirdPartySourceBundle(repo, project, *version, server, tui, container, provenance, *out); err != nil {
			fatal(err)
		}
		return
	}

	files, err := render(repo, project, *version, server, tui, container)
	if err != nil {
		fatal(err)
	}
	if *mode == "check" {
		for _, file := range files {
			want, err := os.ReadFile(filepath.Join(repo, file.path))
			if err != nil {
				fatal(fmt.Errorf("generated file missing: %s: %w", file.path, err))
			}
			if string(want) != string(file.data) {
				fatal(fmt.Errorf("generated file is stale: %s", file.path))
			}
		}
		fmt.Printf("legal check passed: server/browser=%d tui=%d container=%d\n", len(server), len(tui), len(container))
		return
	}
	if *mode != "generate" {
		fatal(fmt.Errorf("unknown mode %q", *mode))
	}
	for _, file := range files {
		path := filepath.Join(repo, file.path)
		if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
			fatal(err)
		}
		if err := os.WriteFile(path, file.data, 0o644); err != nil {
			fatal(err)
		}
	}
	fmt.Printf("legal generated: server/browser=%d tui=%d container=%d\n", len(server), len(tui), len(container))
}

func fatal(err error) {
	fmt.Fprintln(os.Stderr, err)
	os.Exit(1)
}

func thirdPartySourceBundle(repo string, project legal.Project, version string, server, tui, container []legal.Component, provenance []legal.Provenance, output string) error {
	if output == "" {
		output = filepath.Join(repo, "go", "dist", fmt.Sprintf("graphite-meter_%s_third-party-source.tar.gz", version))
	}
	if err := os.MkdirAll(filepath.Dir(output), 0o755); err != nil {
		return err
	}
	archiveRoot := fmt.Sprintf("graphite-meter_%s_third-party-source", version)
	file, err := os.Create(output)
	if err != nil {
		return err
	}
	defer file.Close()
	zipper := gzip.NewWriter(file)
	tarWriter := tar.NewWriter(zipper)
	closeArchive := func() error {
		if err := tarWriter.Close(); err != nil {
			return err
		}
		return zipper.Close()
	}

	seen := map[string]bool{}
	for _, component := range append(append(server, tui...), container...) {
		if component.Ecosystem != "go" || component.Name == "Go standard library" || seen[component.Name+"\x00"+component.Version] {
			continue
		}
		seen[component.Name+"\x00"+component.Version] = true
		dir, err := moduleDirectory(repo, component.Name, component.Version)
		if err != nil {
			return err
		}
		if err := addTree(tarWriter, dir, archiveRoot+"/third_party/go/"+safeName(component.Name+"@"+component.Version)); err != nil {
			return fmt.Errorf("archive Go source %s@%s: %w", component.Name, component.Version, err)
		}
	}
	for _, component := range server {
		if component.Ecosystem != "npm" || seen[component.Name+"\x00"+component.Version] {
			continue
		}
		seen[component.Name+"\x00"+component.Version] = true
		dir := component.SourcePath
		if dir == "" {
			dir = filepath.Join(repo, "client", "node_modules", filepath.FromSlash(component.Name))
		}
		if _, err := os.Stat(dir); err != nil {
			return fmt.Errorf("browser source component %s: %w", component.Name, err)
		}
		if err := addTree(tarWriter, dir, archiveRoot+"/third_party/npm/"+safeName(component.Name+"@"+component.Version)); err != nil {
			return fmt.Errorf("archive browser source %s@%s: %w", component.Name, component.Version, err)
		}
	}
	for _, entry := range provenance {
		base, err := manualSourceDestination(entry)
		if err != nil {
			return err
		}
		base = archiveRoot + "/" + base
		for _, local := range append(append([]string{}, entry.LocalPaths...), legalFilePaths(entry.LocalLegalFiles)...) {
			if filepath.IsAbs(local) {
				continue
			}
			path := filepath.Join(repo, filepath.FromSlash(local))
			if _, err := os.Lstat(path); err != nil {
				if os.IsNotExist(err) {
					continue
				}
				return fmt.Errorf("inspect manual source %s for %s: %w", local, entry.Name, err)
			}
			if err := addTree(tarWriter, path, base+"/"+filepath.Base(path)); err != nil {
				return fmt.Errorf("archive manual source %s for %s: %w", local, entry.Name, err)
			}
		}
	}

	legalInventory := map[string]any{"server": server, "tui": tui, "container": container}
	inventoryData, err := json.MarshalIndent(legalInventory, "", "  ")
	if err != nil {
		return err
	}
	if err := addBytes(tarWriter, archiveRoot+"/LEGAL_INVENTORY.json", append(inventoryData, '\n')); err != nil {
		return err
	}
	provenanceData, err := json.MarshalIndent(provenance, "", "  ")
	if err != nil {
		return err
	}
	if err := addBytes(tarWriter, archiveRoot+"/PROVENANCE.json", append(provenanceData, '\n')); err != nil {
		return err
	}
	readme := fmt.Sprintf(
		"Graphite Meter third-party source for %s.\n\n"+
			"This archive contains source material for third-party components used by the Graphite Meter release and its generated legal inventories. It intentionally does not duplicate Graphite Meter's own repository source.\n\n"+
			"For a published GitHub release, use this archive together with GitHub's automatic Source code (tar.gz) or Source code (zip) archive for the matching release tag. Together they form the source offer for that release.\n\n"+
			"Project source repository: %s\n",
		version,
		project.Repository,
	)
	if err := addBytes(tarWriter, archiveRoot+"/README.txt", []byte(readme)); err != nil {
		return err
	}
	return closeArchive()
}

func manualSourceDestination(entry legal.Provenance) (string, error) {
	if entry.CorrespondingSource == "" {
		return "third_party/manual/" + safeName(entry.Name), nil
	}
	if strings.Contains(entry.CorrespondingSource, "\\") {
		return "", fmt.Errorf("invalid corresponding source path for %s: %q", entry.Name, entry.CorrespondingSource)
	}
	clean := filepath.ToSlash(filepath.Clean(filepath.FromSlash(entry.CorrespondingSource)))
	if filepath.IsAbs(entry.CorrespondingSource) || clean == "." || clean == ".." || strings.HasPrefix(clean, "../") || !strings.HasPrefix(clean, "third_party/manual/") {
		return "", fmt.Errorf("invalid corresponding source path for %s: %q", entry.Name, entry.CorrespondingSource)
	}
	return clean, nil
}

func legalFilePaths(files []legal.LegalFile) []string {
	paths := make([]string, 0, len(files))
	for _, file := range files {
		paths = append(paths, file.Name)
	}
	return paths
}

func moduleDirectory(repo, name, version string) (string, error) {
	cmd := exec.Command("go", "list", "-m", "-json", name)
	cmd.Dir = filepath.Join(repo, "go")
	out, err := cmd.Output()
	if err != nil {
		return "", fmt.Errorf("locate source for %s@%s: %w", name, version, err)
	}
	var module struct{ Path, Version, Dir string }
	if err := json.Unmarshal(out, &module); err != nil {
		return "", err
	}
	if module.Version != version || module.Dir == "" {
		return "", fmt.Errorf("source version mismatch for %s: got %s, want %s", name, module.Version, version)
	}
	return module.Dir, nil
}

func safeName(value string) string {
	return strings.NewReplacer("/", "_", "\\", "_", "@", "_at_").Replace(value)
}

func addTree(writer *tar.Writer, root, destination string) error {
	return filepath.Walk(root, func(path string, info os.FileInfo, walkErr error) error {
		if walkErr != nil {
			return walkErr
		}
		if info.IsDir() {
			return nil
		}
		rel, err := filepath.Rel(root, path)
		if err != nil {
			return err
		}
		data, err := os.ReadFile(path)
		if err != nil {
			return err
		}
		name := destination
		if rel != "." {
			name = filepath.Join(destination, rel)
		}
		return addBytes(writer, filepath.ToSlash(name), data)
	})
}

func addBytes(writer *tar.Writer, name string, data []byte) error {
	header := &tar.Header{Name: filepath.ToSlash(name), Mode: 0o644, Size: int64(len(data)), Uid: 0, Gid: 0, ModTime: time.Time{}}
	if err := writer.WriteHeader(header); err != nil {
		return err
	}
	_, err := io.Copy(writer, bytes.NewReader(data))
	return err
}

func repositoryRoot(explicit string) (string, error) {
	if explicit != "" {
		return filepath.Abs(explicit)
	}
	dir, err := os.Getwd()
	if err != nil {
		return "", err
	}
	for {
		if _, err := os.Stat(filepath.Join(dir, ".git")); err == nil {
			return dir, nil
		}
		parent := filepath.Dir(dir)
		if parent == dir {
			return "", errors.New("could not find repository root")
		}
		dir = parent
	}
}

func discoverGo(repo string, reviews []legal.Review, provenance []legal.Provenance) ([]legal.Component, []legal.Component, error) {
	targets, err := goDiscoveryTargets(repo)
	if err != nil {
		return nil, nil, err
	}
	modules := map[string]struct {
		path, version, dir string
	}{}
	standard := false
	for _, target := range targets {
		cmd := "./cmd/graphite-meter"
		if target.name == "tui" {
			cmd = "./cmd/graphite-meter-client"
		}
		packages, err := legal.RunGoList(repo, cmd, target.goos, target.goarch)
		if err != nil {
			return nil, nil, err
		}
		for _, pkg := range packages {
			if pkg.Standard {
				standard = true
			}
			if pkg.Module == nil || strings.HasPrefix(pkg.Module.Path, "github.com/zR-JB/graphite-meter/go") {
				continue
			}
			path, version, dir := pkg.Module.Path, pkg.Module.Version, pkg.Module.Dir
			if pkg.Module.Replace != nil {
				path, version, dir = pkg.Module.Replace.Path, pkg.Module.Replace.Version, pkg.Module.Replace.Dir
				if dir == "" {
					return nil, nil, fmt.Errorf("local or custom Go replacement has no resolved directory: %s", path)
				}
				if !hasGoReplacementProvenance(provenance, path, version, target.name) {
					return nil, nil, fmt.Errorf("local or custom Go replacement requires provenance: %s", path)
				}
			}
			modules[target.name+"\x00"+path] = struct{ path, version, dir string }{path, version, dir}
		}
	}
	var server, tui []legal.Component
	for key, module := range modules {
		parts := strings.SplitN(key, "\x00", 2)
		component, err := goComponent(module.path, module.version, module.dir, reviews)
		if err != nil {
			return nil, nil, err
		}
		if parts[0] == "server" {
			server = append(server, component)
		} else {
			tui = append(tui, component)
		}
	}
	if standard {
		toolchain, err := goToolchainComponent(repo)
		if err != nil {
			return nil, nil, err
		}
		server = append(server, toolchain)
		tui = append(tui, toolchain)
	}
	return sortComponents(server), sortComponents(tui), nil
}

func goDiscoveryTargets(repo string) ([]goTarget, error) {
	data, err := os.ReadFile(filepath.Join(repo, "scripts", "tui-targets.txt"))
	if err != nil {
		return nil, fmt.Errorf("read TUI target list: %w", err)
	}
	var targets []goTarget
	seen := make(map[string]bool)
	for line := range strings.SplitSeq(string(data), "\n") {
		line = strings.TrimSpace(line)
		if line == "" || strings.HasPrefix(line, "#") {
			continue
		}
		fields := slices.Collect(strings.SplitSeq(line, "/"))
		if len(fields) != 2 || fields[0] == "" || fields[1] == "" || seen[line] {
			return nil, fmt.Errorf("invalid or duplicate TUI target %q", line)
		}
		seen[line] = true
		targets = append(targets, goTarget{"tui", fields[0], fields[1]})
	}
	if len(targets) == 0 {
		return nil, errors.New("scripts/tui-targets.txt contains no targets")
	}
	serverTargets := make([]goTarget, 0, 2)
	for _, target := range targets {
		if target.goos == "linux" && (target.goarch == "amd64" || target.goarch == "arm64") {
			serverTargets = append(serverTargets, goTarget{"server", target.goos, target.goarch})
		}
	}
	if len(serverTargets) != 2 {
		return nil, errors.New("scripts/tui-targets.txt must include linux/amd64 and linux/arm64")
	}
	return append(serverTargets, targets...), nil
}

func hasGoReplacementProvenance(entries []legal.Provenance, name, version, scope string) bool {
	for _, entry := range entries {
		if entry.Ecosystem != "go" || entry.Name != name || entry.Version == "" {
			continue
		}
		if !contains(entry.ArtifactScopes, scope) && !(scope == "server" && contains(entry.ArtifactScopes, "server/browser")) {
			continue
		}
		if version == "" || entry.Version == version {
			return true
		}
	}
	return false
}

func goComponent(name, version, dir string, reviews []legal.Review) (legal.Component, error) {
	files, err := componentLegalFiles(dir, "go", name, reviews)
	if err != nil {
		// Review-template mode needs to report the unresolved component rather
		// than hiding it behind discovery failure. Normal mode still fails
		// closed because UNKNOWN has no valid review basis.
		return legal.Component{Name: name, Version: version, Ecosystem: "go", Source: legalSource(name, ""), DeclaredLicenseExpression: "UNKNOWN", SelectedLicenseExpression: "UNKNOWN"}, nil
	}
	component := componentFromFiles("go", name, version, legalSource(name, ""), files)
	for _, review := range reviews {
		if review.Ecosystem == "go" && review.Name == name {
			component.DeclaredLicenseExpression = review.DeclaredLicenseExpression
			component.SelectedLicenseExpression = review.SelectedLicenseExpression
			break
		}
	}
	return component, nil
}

func componentLegalFiles(dir, ecosystem, name string, reviews []legal.Review) ([]legal.LegalFile, error) {
	discovered, discoverErr := legal.ReadLegalFiles(dir)
	review := findReview(ecosystem, name, reviews)
	if review == nil {
		return discovered, discoverErr
	}
	if discoverErr != nil && !strings.Contains(discoverErr.Error(), "no legal candidate") {
		return nil, discoverErr
	}
	files, err := reviewedLegalFiles(dir, ecosystem, name, reviews)
	if err != nil {
		return nil, err
	}
	if discoverErr == nil {
		reviewedNames := make(map[string]bool, len(review.LegalFiles))
		for _, file := range review.LegalFiles {
			reviewedNames[strings.ToLower(filepath.ToSlash(file.Name))] = true
		}
		for _, file := range discovered {
			if !reviewedNames[strings.ToLower(filepath.ToSlash(file.Name))] {
				return nil, fmt.Errorf("LEGAL REVIEW REQUIRED: new legal file for %s: %s", name, file.Name)
			}
		}
	}
	return files, nil
}

func findReview(ecosystem, name string, reviews []legal.Review) *legal.Review {
	for i := range reviews {
		if reviews[i].Ecosystem == ecosystem && reviews[i].Name == name {
			return &reviews[i]
		}
	}
	return nil
}

func reviewedLegalFiles(dir, ecosystem, name string, reviews []legal.Review) ([]legal.LegalFile, error) {
	review := findReview(ecosystem, name, reviews)
	if review == nil || len(review.LegalFiles) == 0 {
		return nil, errors.New("no explicit reviewed legal-file override")
	}
	files := make([]legal.LegalFile, 0, len(review.LegalFiles))
	for _, expected := range review.LegalFiles {
		relative := filepath.Clean(filepath.FromSlash(expected.Name))
		if filepath.IsAbs(relative) || relative == "." || relative == ".." || strings.HasPrefix(relative, ".."+string(filepath.Separator)) {
			return nil, fmt.Errorf("reviewed legal path must stay inside component: %s", expected.Name)
		}
		path := filepath.Join(dir, relative)
		data, err := os.ReadFile(path)
		if os.IsNotExist(err) {
			// Bun's isolated linker stores a package's dependencies beside the
			// package directory rather than beneath package/node_modules. Preserve
			// the reviewed logical name while reading the same dependency-owned
			// legal file from that isolated-store sibling.
			prefix := "node_modules" + string(filepath.Separator)
			if after, ok := strings.CutPrefix(relative, prefix); ok {
				path = filepath.Join(filepath.Dir(dir), after)
				data, err = os.ReadFile(path)
			}
		}
		if err != nil {
			return nil, err
		}
		file := expected
		file.SHA256 = legal.SHA256(data)
		file.Text = string(data)
		if file.Kind == "" {
			file.Kind = legal.LegalFileKind(file.Name)
		}
		files = append(files, file)
	}
	return files, nil
}

func goToolchainComponent(repo string) (legal.Component, error) {
	// The reviewed standard-library version is a repository property, not an
	// ambient PATH property. The generator itself is built by `go run`, so
	// runtime.Version reports the toolchain that actually compiled/executed it.
	// Require that selected toolchain to match the exact release in go/go.mod,
	// then use the repository pin as the inventory version. Go 1.27's tidy
	// removes a redundant toolchain directive when it equals the go directive,
	// so the patch-qualified go directive is authoritative in that layout.
	//
	// This deliberately avoids spawning a second `go env GOVERSION` process:
	// hooks, shims, and linked worktrees can make that nested command resolve a
	// different Go executable even though the generator is already running with
	// the correct selected toolchain.
	expected, err := repositoryGoToolchainVersion(repo)
	if err != nil {
		return legal.Component{}, err
	}
	actual := legalGoVersion(runtime.Version())
	if err := validateGoToolchainVersion(expected, actual); err != nil {
		return legal.Component{}, err
	}

	// The checked-in snapshots are the canonical legal material. GOROOT is an
	// installation-dependent input and may contain distro or local-build
	// formatting differences that are unrelated to the reviewed Go release.
	files, err := legal.ReadRootLegalFiles(filepath.Join(repo, "legal", "toolchains", "go"))
	if err != nil {
		return legal.Component{}, fmt.Errorf("go toolchain legal material unavailable: %w", err)
	}
	return componentFromFiles("go-toolchain", "Go standard library", expected, "https://go.dev/", files), nil
}

func repositoryGoToolchainVersion(repo string) (string, error) {
	data, err := os.ReadFile(filepath.Join(repo, "go", "go.mod"))
	if err != nil {
		return "", fmt.Errorf("read pinned Go toolchain: %w", err)
	}
	var languageVersion string
	for line := range strings.SplitSeq(string(data), "\n") {
		fields := strings.Fields(line)
		if len(fields) == 0 {
			continue
		}
		if fields[0] == "go" {
			if len(fields) != 2 || !goDirectiveVersion.MatchString(fields[1]) {
				return "", fmt.Errorf("go/go.mod must pin an exact Go release, got %q", strings.Join(fields[1:], " "))
			}
			languageVersion = "go" + fields[1]
			continue
		}
		if fields[0] == "toolchain" {
			if len(fields) != 2 {
				return "", fmt.Errorf("go/go.mod has malformed toolchain directive")
			}
			match := goReleaseVersion.FindStringSubmatch(fields[1])
			if match == nil || match[1] != fields[1] {
				return "", fmt.Errorf("go/go.mod toolchain must pin an exact Go release, got %q", fields[1])
			}
			return match[1], nil
		}
	}
	if languageVersion != "" {
		return languageVersion, nil
	}
	return "", fmt.Errorf("go/go.mod must declare an exact Go release for legal review")
}

func validateGoToolchainVersion(expected, actual string) error {
	if actual != expected {
		return fmt.Errorf(
			"go toolchain mismatch during legal review: go/go.mod pins %s but legalgen is running %s; run `just doctor`",
			expected,
			actual,
		)
	}
	return nil
}

func legalGoVersion(raw string) string {
	if match := goReleaseVersion.FindStringSubmatch(raw); match != nil {
		return match[1]
	}
	return raw
}

func legalSource(name, upstream string) string {
	return sourceFor(name, upstream)
}

func sourceFor(name, upstream string) string {
	if upstream != "" {
		return upstream
	}
	parts := strings.Split(name, "/")
	if len(parts) >= 3 && (parts[0] == "github.com" || parts[0] == "gitlab.com") {
		return "https://" + strings.Join(parts[:3], "/")
	}
	if len(parts) == 3 && parts[0] == "golang.org" && parts[1] == "x" {
		return "https://go.googlesource.com/" + parts[2]
	}
	return ""
}

func componentFromFiles(ecosystem, name, version, source string, files []legal.LegalFile) legal.Component {
	licenseExpression := inferLicense(files)
	component := legal.Component{Name: name, Version: version, Ecosystem: ecosystem, Source: source, DeclaredLicenseExpression: licenseExpression, SelectedLicenseExpression: licenseExpression, Modified: false}
	for _, file := range files {
		if file.Kind == "notice" {
			component.Notices = append(component.Notices, file)
		} else {
			component.LegalTexts = append(component.LegalTexts, file)
		}
	}
	return component
}

func inferLicense(files []legal.LegalFile) string {
	for _, file := range files {
		text := strings.ToUpper(file.Text)
		switch {
		case strings.Contains(text, "APACHE LICENSE"):
			return "Apache-2.0"
		case strings.Contains(text, "MIT LICENSE"), strings.Contains(text, "PERMISSION IS HEREBY GRANTED, FREE OF CHARGE"):
			return "MIT"
		case strings.Contains(text, "PERMISSION TO USE, COPY, MODIFY, AND DISTRIBUTE"):
			return "ISC"
		case strings.Contains(text, "ISC LICENSE"):
			return "ISC"
		case strings.Contains(text, "BSD 3-CLAUSE"), strings.Contains(text, "REDISTRIBUTION AND USE IN SOURCE AND BINARY FORMS"):
			return "BSD-3-Clause"
		case strings.Contains(text, "BSD 2-CLAUSE"):
			return "BSD-2-Clause"
		}
	}
	return "UNKNOWN"
}

func discoverBrowser(repo, scanPath string, reviews []legal.Review) ([]legal.Component, error) {
	var ids []string
	if err := legal.ReadJSON(scanPath, &ids); err != nil {
		return nil, fmt.Errorf("read browser scan: %w", err)
	}
	roots := map[string]struct{}{}
	for _, id := range ids {
		if !strings.Contains(filepath.ToSlash(id), "/node_modules/") {
			continue
		}
		root, err := packageRoot(id)
		if err != nil {
			return nil, err
		}
		roots[root] = struct{}{}
	}
	var result []legal.Component
	for root := range roots {
		component, err := npmComponent(root, reviews)
		if err != nil {
			return nil, err
		}
		result = append(result, component)
	}
	return sortComponents(result), nil
}

func packageRoot(moduleID string) (string, error) {
	id := filepath.Clean(strings.TrimPrefix(moduleID, "\\x00"))
	if !filepath.IsAbs(id) {
		id, _ = filepath.Abs(id)
	}
	for dir := id; ; dir = filepath.Dir(dir) {
		if _, err := os.Stat(filepath.Join(dir, "package.json")); err == nil {
			return dir, nil
		}
		parent := filepath.Dir(dir)
		if parent == dir {
			return "", fmt.Errorf("bundled browser module has no package.json root: %s", moduleID)
		}
	}
}

func npmComponent(root string, reviews []legal.Review) (legal.Component, error) {
	var metadata struct {
		Name     string `json:"name"`
		Version  string `json:"version"`
		License  any    `json:"license"`
		Licenses []struct {
			Type string `json:"type"`
		} `json:"licenses"`
		Repository any `json:"repository"`
	}
	if err := legal.ReadJSON(filepath.Join(root, "package.json"), &metadata); err != nil {
		return legal.Component{}, err
	}
	files, err := componentLegalFiles(root, "npm", metadata.Name, reviews)
	if err != nil {
		return legal.Component{}, fmt.Errorf("npm package %s: %w", metadata.Name, err)
	}
	component := componentFromFiles("npm", metadata.Name, metadata.Version, repositoryURL(metadata.Repository, metadata.Name), files)
	component.SourcePath = root
	component.DeclaredLicenseExpression = packageLicense(metadata.License, metadata.Licenses, component.DeclaredLicenseExpression)
	component.SelectedLicenseExpression = component.DeclaredLicenseExpression
	return component, nil
}

func applyReviewedSelections(components []legal.Component, reviews []legal.Review) []legal.Component {
	result := slices.Clone(components)
	for i := range result {
		for _, review := range reviews {
			if review.Ecosystem == result[i].Ecosystem && review.Name == result[i].Name {
				result[i].SelectedLicenseExpression = review.SelectedLicenseExpression
				break
			}
		}
	}
	return result
}

func packageLicense(value any, licenses []struct {
	Type string `json:"type"`
}, fallback string) string {
	if text, ok := value.(string); ok && text != "" {
		return text
	}
	for _, item := range licenses {
		if item.Type != "" {
			return item.Type
		}
	}
	return fallback
}

func repositoryURL(value any, name string) string {
	if text, ok := value.(string); ok {
		return strings.TrimSuffix(strings.TrimPrefix(text, "git+"), ".git")
	}
	if object, ok := value.(map[string]any); ok {
		if text, ok := object["url"].(string); ok {
			return strings.TrimSuffix(strings.TrimPrefix(text, "git+"), ".git")
		}
	}
	return sourceFor(name, "")
}

func addProvenance(repo string, components []legal.Component, entries []legal.Provenance, scope string) ([]legal.Component, error) {
	for _, entry := range entries {
		if !contains(entry.ArtifactScopes, scope) && !(scope == "server/browser" && contains(entry.ArtifactScopes, "server")) {
			continue
		}
		if entry.Name == "" || entry.Version == "" || entry.LicenseExpression == "" || strings.Contains(strings.ToUpper(entry.LicenseExpression), "UNKNOWN") || entry.ReviewNotes == "" {
			return nil, fmt.Errorf("provenance entry %q is incomplete or unresolved", entry.Name)
		}
		for _, artifact := range entry.LocalArtifacts {
			if artifact.Path == "" || artifact.SHA256 == "" {
				return nil, fmt.Errorf("provenance %s has an incomplete local artifact", entry.Name)
			}
			path := artifact.Path
			if !filepath.IsAbs(path) {
				path = filepath.Join(repo, filepath.FromSlash(path))
			}
			data, err := os.ReadFile(path)
			if err != nil {
				return nil, fmt.Errorf("provenance %s local artifact %s: %w", entry.Name, artifact.Path, err)
			}
			if artifact.SHA256 != legal.SHA256(data) {
				return nil, fmt.Errorf("provenance %s local artifact hash changed: %s", entry.Name, artifact.Path)
			}
		}
		files := slices.Clone(entry.LocalLegalFiles)
		for i := range files {
			path := filepath.Join(repo, filepath.FromSlash(files[i].Name))
			data, err := os.ReadFile(path)
			if err != nil {
				return nil, fmt.Errorf("provenance %s legal file %s: %w", entry.Name, files[i].Name, err)
			}
			if files[i].SHA256 != legal.SHA256(data) {
				return nil, fmt.Errorf("provenance %s legal file hash changed: %s", entry.Name, files[i].Name)
			}
			files[i].Text = string(data)
			if files[i].Kind == "" {
				files[i].Kind = "license"
			}
			// Provenance paths identify the checked-in source of a manual
			// artifact; notices expose the upstream/legal basename instead of a
			// workstation-relative repository path.
			files[i].Name = filepath.Base(files[i].Name)
		}
		component := legal.Component{Name: entry.Name, Version: entry.Version, Ecosystem: entry.Ecosystem, Source: entry.Upstream, DeclaredLicenseExpression: entry.LicenseExpression, SelectedLicenseExpression: entry.LicenseExpression, Modified: entry.Modified}
		for _, file := range files {
			if file.Kind == "notice" {
				component.Notices = append(component.Notices, file)
			} else {
				component.LegalTexts = append(component.LegalTexts, file)
			}
		}
		components = append(components, component)
	}
	return sortComponents(components), nil
}

func contains(values []string, want string) bool {
	return slices.Contains(values, want)
}

func sortComponents(components []legal.Component) []legal.Component {
	seen := map[string]legal.Component{}
	for _, component := range components {
		seen[component.Ecosystem+"\x00"+component.Name+"\x00"+component.Version] = component
	}
	result := make([]legal.Component, 0, len(seen))
	for _, component := range seen {
		result = append(result, component)
	}
	slices.SortFunc(result, func(a, b legal.Component) int {
		left := a.Ecosystem + "\x00" + a.Name + "\x00" + a.Version
		right := b.Ecosystem + "\x00" + b.Name + "\x00" + b.Version
		return cmp.Compare(left, right)
	})
	return result
}

func render(repo string, project legal.Project, version string, server, tui, container []legal.Component) ([]outputFile, error) {
	sourceURL := project.Repository
	if releaseVersion.MatchString(version) {
		version = strings.TrimPrefix(version, "v")
		sourceURL += "/tree/v" + version
	}
	licenseText, err := os.ReadFile(filepath.Join(repo, "LICENSE"))
	if err != nil {
		return nil, err
	}
	copyText := fmt.Sprintf("%s\nCopyright © %s %s\n\n%s is free software licensed under %s.\nSee LICENSE for the complete GNU Affero General Public License version 3 text.\n", project.Name, project.CopyrightYears, project.CopyrightHolder, project.Name, project.LicenseExpression)
	files := []outputFile{{"COPYRIGHT", []byte(copyText)}}
	for _, scope := range []struct {
		name       string
		components []legal.Component
	}{{"server", server}, {"tui", tui}, {"container", container}} {
		inv, err := marshal(inventory{SchemaVersion: 1, Scope: scope.name, Components: scope.components})
		if err != nil {
			return nil, err
		}
		files = append(files, outputFile{filepath.Join("legal", "generated", scope.name, "inventory.json"), inv})
		files = append(files, outputFile{filepath.Join("legal", "generated", scope.name, "THIRD_PARTY_NOTICES.txt"), []byte(notices(scope.components))})
		files = append(files, outputFile{filepath.Join("legal", "generated", scope.name, "SOURCE.txt"), []byte(sourceURL + "\n")})
	}
	web, err := marshal(about{
		SchemaVersion: 2,
		Project:       project,
		SourceVersion: version,
		SourceURL:     sourceURL,
		LicenseURL:    "legal/LICENSE.txt",
		NoticesURL:    "legal/THIRD_PARTY_NOTICES.txt",
		Components:    aboutComponents(server),
	})
	if err != nil {
		return nil, err
	}
	files = append(files, outputFile{"client/public/legal/about.json", web})
	files = append(files, outputFile{"client/public/legal/LICENSE.txt", licenseText})
	files = append(files, outputFile{"client/public/legal/THIRD_PARTY_NOTICES.txt", []byte(notices(server))})
	tuiNotices := notices(tui)
	files = append(files, outputFile{filepath.Join("go", "internal", "legal", "assets", "TUI_LEGAL.txt"), []byte(tuiReport(copyText, sourceURL, licenseText, tuiNotices))})
	return files, nil
}

func aboutComponents(components []legal.Component) []aboutComponent {
	result := make([]aboutComponent, 0, len(components))
	for _, component := range components {
		result = append(result, aboutComponent{
			Name:                      component.Name,
			Version:                   component.Version,
			Ecosystem:                 component.Ecosystem,
			Source:                    component.Source,
			DeclaredLicenseExpression: component.DeclaredLicenseExpression,
			SelectedLicenseExpression: component.SelectedLicenseExpression,
			Modified:                  component.Modified,
		})
	}
	return result
}

func tuiReport(copyText, sourceURL string, licenseText []byte, noticesText string) string {
	var out strings.Builder
	out.WriteString(copyText)
	fmt.Fprintf(&out, "\nSource code: %s\n\n", sourceURL)
	out.WriteString("LICENSE\n\n")
	out.Write(licenseText)
	if len(licenseText) == 0 || licenseText[len(licenseText)-1] != '\n' {
		out.WriteByte('\n')
	}
	out.WriteString("\n")
	out.WriteString(noticesText)
	return out.String()
}

func marshal(value any) ([]byte, error) {
	b, err := json.MarshalIndent(value, "", "  ")
	if err != nil {
		return nil, err
	}
	return append(b, '\n'), nil
}

func notices(components []legal.Component) string {
	var out strings.Builder
	out.WriteString("THIRD-PARTY SOFTWARE NOTICES\n\nGraphite Meter includes third-party software described below.\n\n")
	for _, component := range components {
		fmt.Fprintf(&out, "Component: %s\nVersion: %s\nSource: %s\nLicense: %s\nModified by Graphite Meter: %s\n\n", component.Name, component.Version, component.Source, component.SelectedLicenseExpression, yesNo(component.Modified))
		for _, file := range component.LegalTexts {
			fmt.Fprintf(&out, "--- %s ---\n\n%s\n", file.Name, file.Text)
		}
		for _, file := range component.Notices {
			fmt.Fprintf(&out, "--- %s ---\n\n%s\n", file.Name, file.Text)
		}
		out.WriteString("============================================================\n\n")
	}
	return out.String()
}

func yesNo(value bool) string {
	if value {
		return "yes"
	}
	return "no"
}

func printReviewTemplate(server, tui, container []legal.Component, reviews []legal.Review) {
	seen := map[string]legal.Component{}
	for _, component := range append(append(server, tui...), container...) {
		seen[component.Ecosystem+"\x00"+component.Name] = component
	}
	var out []map[string]any
	for _, component := range sortComponents(values(seen)) {
		files := append(append([]legal.LegalFile{}, component.LegalTexts...), component.Notices...)
		out = append(out, map[string]any{
			"ecosystem": component.Ecosystem, "name": component.Name, "reviewedVersion": component.Version,
			"upstream": component.Source, "declaredLicenseExpression": component.DeclaredLicenseExpression,
			"selectedLicenseExpression": "", "legalFiles": files, "reviewDecision": "", "reviewNotes": "",
		})
	}
	b, _ := marshal(out)
	fmt.Print(string(b))
}

func printReviewAudit(server, tui, container []legal.Component, reviews []legal.Review) {
	type auditComponent struct {
		Scope       string          `json:"scope"`
		Component   legal.Component `json:"component"`
		Review      *legal.Review   `json:"review,omitempty"`
		ReviewState string          `json:"reviewState"`
	}
	var out []auditComponent
	for _, scoped := range []struct {
		name       string
		components []legal.Component
	}{
		{"server/browser", server}, {"tui", tui}, {"container", container},
	} {
		for _, component := range scoped.components {
			var matched *legal.Review
			for i := range reviews {
				if reviews[i].Ecosystem == component.Ecosystem && reviews[i].Name == component.Name {
					matched = &reviews[i]
					break
				}
			}
			state := "review-required"
			if matched != nil && legal.ValidateReview(component, []legal.Review{*matched}) == nil {
				state = "fingerprint-matches"
			}
			out = append(out, auditComponent{Scope: scoped.name, Component: component, Review: matched, ReviewState: state})
		}
	}
	b, err := marshal(out)
	if err != nil {
		fatal(err)
	}
	fmt.Print(string(b))
}

func values(values map[string]legal.Component) []legal.Component {
	return slices.Collect(maps.Values(values))
}
