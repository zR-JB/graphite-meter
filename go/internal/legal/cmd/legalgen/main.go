package main

import (
	"archive/tar"
	"cmp"
	"compress/gzip"
	"encoding/json/jsontext"
	"encoding/json/v2"
	"errors"
	"flag"
	"fmt"
	"maps"
	"os"
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

type componentScope struct {
	name       string
	components []legal.Component
}

type packageMetadata struct {
	Name     string `json:"name"`
	Version  string `json:"version"`
	License  any    `json:"license"`
	Licenses []struct {
		Type string `json:"type"`
	} `json:"licenses"`
	Repository any `json:"repository"`
}

type discoveredModule struct {
	scope, path, version, dir string
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

	repo := must(repositoryRoot(*repoFlag))
	*version = cmp.Or(*version, "development")

	project := must(legal.ReadProject(repo))
	reviews := must(legal.ReadReviews(repo))
	provenance := must(legal.ReadProvenance(repo))
	serverGo, tuiGo, err := discoverGo(repo, reviews, provenance)
	check(err)
	*browserScan = cmp.Or(*browserScan, os.Getenv("GM_LEGAL_SCAN_MODULES"))
	if *browserScan == "" {
		fatal(errors.New("browser scan output is required; run the temporary production Vite scan first"))
	}
	browser := must(discoverBrowser(repo, *browserScan, reviews))
	sets := []componentScope{{"server/browser", slices.Concat(serverGo, browser)}, {"tui", slices.Clone(tuiGo)}}
	for i, scope := range []string{"server/browser", "tui"} {
		sets[i].components = must(addProvenance(repo, sets[i].components, provenance, scope))
	}
	sets = append(sets, componentScope{"container", must(addProvenance(repo, slices.Clone(sets[0].components), provenance, "container"))})
	check(prepareScopes(sets, reviews, *mode))
	server, tui, container := sets[0].components, sets[1].components, sets[2].components
	switch *mode {
	case "review-template":
		printReviewTemplate(server, tui, container)
		return
	case "review-audit":
		printReviewAudit(server, tui, container, reviews)
		return
	case "third-party-source-bundle":
		check(thirdPartySourceBundle(repo, project, *version, server, tui, container, provenance, *out))
		return
	case "check", "generate":
	default:
		fatal(fmt.Errorf("unknown mode %q", *mode))
	}

	files := must(render(repo, project, *version, server, tui, container))
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
	refreshReviewedVersions(sets, reviews)
	reviewData := must(marshal(reviews))
	files = append(files, outputFile{filepath.Join("legal", "reviewed-components.json"), reviewData})
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

func refreshReviewedVersions(scopes []componentScope, reviews []legal.Review) {
	versions := make(map[string]string)
	for _, scope := range scopes {
		for _, component := range scope.components {
			versions[component.Ecosystem+"\x00"+component.Name] = component.Version
		}
	}
	for i := range reviews {
		if version := versions[reviews[i].Ecosystem+"\x00"+reviews[i].Name]; version != "" {
			reviews[i].ReviewedVersion = version
		}
	}
}

func fatal(err error) {
	fmt.Fprintln(os.Stderr, err)
	os.Exit(1)
}

func check(err error) {
	if err != nil {
		fatal(err)
	}
}

func must[T any](value T, err error) T {
	check(err)
	return value
}

func prepareScopes(scopes []componentScope, reviews []legal.Review, mode string) error {
	for i := range scopes {
		for _, component := range scopes[i].components {
			if err := legal.ValidateReview(component, reviews); err != nil && mode != "review-template" && mode != "review-audit" {
				return fmt.Errorf("%s: %w", scopes[i].name, err)
			}
		}
		components := slices.Clone(scopes[i].components)
		for j := range components {
			if review := findReview(components[j].Ecosystem, components[j].Name, reviews); review != nil {
				components[j].SelectedLicenseExpression = review.SelectedLicenseExpression
			}
		}
		scopes[i].components = components
	}
	return nil
}

func thirdPartySourceBundle(repo string, project legal.Project, version string, server, tui, container []legal.Component, provenance []legal.Provenance, output string) error {
	output = cmp.Or(output, filepath.Join(repo, "go", "dist", fmt.Sprintf("graphite-meter_%s_third-party-source.tar.gz", version)))
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
	if err := archiveComponentSources(tarWriter, archiveRoot+"/third_party/go/", slices.Concat(server, tui, container), "go", seen); err != nil {
		return err
	}
	if err := archiveComponentSources(tarWriter, archiveRoot+"/third_party/npm/", server, "npm", seen); err != nil {
		return err
	}
	for _, entry := range provenance {
		base, err := manualSourceDestination(entry)
		if err != nil {
			return err
		}
		base = archiveRoot + "/" + base
		localPaths := slices.Clone(entry.LocalPaths)
		for _, file := range entry.LocalLegalFiles {
			localPaths = append(localPaths, file.Name)
		}
		for _, local := range localPaths {
			if filepath.IsAbs(local) {
				continue
			}
			path := filepath.Join(repo, filepath.FromSlash(local))
			if _, err := os.Lstat(path); err != nil {
				return fmt.Errorf("inspect manual source %s for %s: %w", local, entry.Name, err)
			}
			if err := addTree(tarWriter, path, base+"/"+filepath.Base(path)); err != nil {
				return fmt.Errorf("archive manual source %s for %s: %w", local, entry.Name, err)
			}
		}
	}

	if err := addJSON(tarWriter, archiveRoot+"/LEGAL_INVENTORY.json", map[string]any{"server": server, "tui": tui, "container": container}); err != nil {
		return err
	}
	if err := addJSON(tarWriter, archiveRoot+"/PROVENANCE.json", provenance); err != nil {
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

func archiveComponentSources(writer *tar.Writer, destinationPrefix string, components []legal.Component, ecosystem string, seen map[string]bool) error {
	for _, component := range components {
		key := component.Name + "\x00" + component.Version
		if component.Ecosystem != ecosystem || seen[key] {
			continue
		}
		seen[key] = true
		if component.SourcePath == "" {
			return fmt.Errorf("source directory unavailable for %s %s@%s", ecosystem, component.Name, component.Version)
		}
		destination := destinationPrefix + safeName(component.Name+"@"+component.Version)
		if err := addTree(writer, component.SourcePath, destination); err != nil {
			return fmt.Errorf("archive %s source %s@%s: %w", ecosystem, component.Name, component.Version, err)
		}
	}
	return nil
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
		return addBytes(writer, filepath.ToSlash(filepath.Join(destination, rel)), data)
	})
}

func addBytes(writer *tar.Writer, name string, data []byte) error {
	header := &tar.Header{Name: filepath.ToSlash(name), Mode: 0o644, Size: int64(len(data)), Uid: 0, Gid: 0, ModTime: time.Time{}}
	if err := writer.WriteHeader(header); err != nil {
		return err
	}
	_, err := writer.Write(data)
	return err
}

func addJSON(writer *tar.Writer, name string, value any) error {
	data, err := marshal(value)
	if err != nil {
		return err
	}
	return addBytes(writer, name, data)
}

func repositoryRoot(explicit string) (string, error) {
	if explicit != "" {
		return filepath.Abs(explicit)
	}
	dir, err := os.Getwd()
	if err != nil {
		return "", err
	}
	for dir := dir; ; dir = filepath.Dir(dir) {
		if _, err := os.Stat(filepath.Join(dir, ".git")); err == nil {
			return dir, nil
		}
		parent := filepath.Dir(dir)
		if parent == dir {
			return "", errors.New("could not find repository root")
		}
	}
}

func discoverGo(repo string, reviews []legal.Review, provenance []legal.Provenance) ([]legal.Component, []legal.Component, error) {
	targets, err := goDiscoveryTargets(repo)
	if err != nil {
		return nil, nil, err
	}
	modules := map[string]discoveredModule{}
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
			modules[target.name+"\x00"+path] = discoveredModule{target.name, path, version, dir}
		}
	}
	var server, tui []legal.Component
	for _, module := range modules {
		files, err := componentLegalFiles(module.dir, "go", module.path, reviews)
		component := legal.Component{}
		if err != nil {
			// Review-template mode needs to report the unresolved component rather than hiding it behind discovery failure.
			component = legal.Component{Name: module.path, Version: module.version, Ecosystem: "go", Source: sourceFor(module.path, ""), DeclaredLicenseExpression: "UNKNOWN", SelectedLicenseExpression: "UNKNOWN"}
		} else {
			component = componentFromFiles("go", module.path, module.version, sourceFor(module.path, ""), files)
			if review := findReview("go", module.path, reviews); review != nil {
				component.DeclaredLicenseExpression = review.DeclaredLicenseExpression
				component.SelectedLicenseExpression = review.SelectedLicenseExpression
			}
		}
		component.SourcePath = module.dir
		if module.scope == "server" {
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
		goos, goarch, ok := strings.Cut(line, "/")
		if !ok || goos == "" || goarch == "" || strings.Contains(goarch, "/") || seen[line] {
			return nil, fmt.Errorf("invalid or duplicate TUI target %q", line)
		}
		seen[line] = true
		targets = append(targets, goTarget{"tui", goos, goarch})
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
	return slices.Concat(serverTargets, targets), nil
}

func hasGoReplacementProvenance(entries []legal.Provenance, name, version, scope string) bool {
	for _, entry := range entries {
		inScope := slices.Contains(entry.ArtifactScopes, scope) || scope == "server" && slices.Contains(entry.ArtifactScopes, "server/browser")
		if entry.Ecosystem == "go" && entry.Name == name && entry.Version != "" && inScope && (version == "" || entry.Version == version) {
			return true
		}
	}
	return false
}

func componentLegalFiles(dir, ecosystem, name string, reviews []legal.Review) ([]legal.LegalFile, error) {
	discovered, discoverErr := legal.ReadLegalFiles(dir)
	review := findReview(ecosystem, name, reviews)
	if review == nil {
		return discovered, discoverErr
	}
	if discoverErr != nil {
		if strings.Contains(discoverErr.Error(), "no legal candidate") {
			return reviewedLegalFiles(dir, ecosystem, name, reviews)
		}
		return nil, discoverErr
	}
	files, err := reviewedLegalFiles(dir, ecosystem, name, reviews)
	if err != nil {
		return nil, err
	}
	reviewedNames := make(map[string]bool, len(review.LegalFiles))
	for _, file := range review.LegalFiles {
		reviewedNames[strings.ToLower(filepath.ToSlash(file.Name))] = true
	}
	for _, file := range discovered {
		if !reviewedNames[strings.ToLower(filepath.ToSlash(file.Name))] {
			return nil, fmt.Errorf("LEGAL REVIEW REQUIRED: new legal file for %s: %s", name, file.Name)
		}
	}
	return files, nil
}

func findReview(ecosystem, name string, reviews []legal.Review) *legal.Review {
	i := slices.IndexFunc(reviews, func(review legal.Review) bool {
		return review.Ecosystem == ecosystem && review.Name == name
	})
	if i < 0 {
		return nil
	}
	return &reviews[i]
}

func reviewedLegalFiles(dir, ecosystem, name string, reviews []legal.Review) ([]legal.LegalFile, error) {
	review := findReview(ecosystem, name, reviews)
	if review == nil || len(review.LegalFiles) == 0 {
		return nil, errors.New("no explicit reviewed legal-file override")
	}
	files := make([]legal.LegalFile, 0, len(review.LegalFiles))
	for _, file := range review.LegalFiles {
		relative := filepath.Clean(filepath.FromSlash(file.Name))
		if filepath.IsAbs(relative) || relative == "." || relative == ".." || strings.HasPrefix(relative, ".."+string(filepath.Separator)) {
			return nil, fmt.Errorf("reviewed legal path must stay inside component: %s", file.Name)
		}
		path := filepath.Join(dir, relative)
		data, err := os.ReadFile(path)
		if os.IsNotExist(err) {
			// Bun's isolated linker stores a package's dependencies beside the package directory rather than beneath.
			if after, ok := strings.CutPrefix(relative, "node_modules"+string(filepath.Separator)); ok {
				data, err = os.ReadFile(filepath.Join(filepath.Dir(dir), after))
			}
		}
		if err != nil {
			return nil, err
		}
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
	// The reviewed standard-library version is a repository property, not an ambient PATH property.
	expected, err := repositoryGoToolchainVersion(repo)
	if err != nil {
		return legal.Component{}, err
	}
	actual := legalGoVersion(runtime.Version())
	if err := validateGoToolchainVersion(expected, actual); err != nil {
		return legal.Component{}, err
	}

	// The checked-in snapshots are the canonical legal material.
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
		switch fields[0] {
		case "go":
			if len(fields) != 2 || !goDirectiveVersion.MatchString(fields[1]) {
				return "", fmt.Errorf("go/go.mod must pin an exact Go release, got %q", strings.Join(fields[1:], " "))
			}
			languageVersion = "go" + fields[1]
		case "toolchain":
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

func sourceFor(name, upstream string) string {
	if upstream != "" {
		return upstream
	}
	parts := strings.Split(name, "/")
	if len(parts) >= 3 {
		switch parts[0] {
		case "github.com", "gitlab.com":
			return "https://" + strings.Join(parts[:3], "/")
		case "golang.org":
			if parts[1] == "x" && len(parts) == 3 {
				return "https://go.googlesource.com/" + parts[2]
			}
		}
	}
	return ""
}

func componentFromFiles(ecosystem, name, version, source string, files []legal.LegalFile) legal.Component {
	licenseExpression := inferLicense(files)
	component := legal.Component{Name: name, Version: version, Ecosystem: ecosystem, Source: source, DeclaredLicenseExpression: licenseExpression, SelectedLicenseExpression: licenseExpression}
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
	var metadata packageMetadata
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
	var text string
	switch repository := value.(type) {
	case string:
		text = repository
	case map[string]any:
		text, _ = repository["url"].(string)
	}
	if text == "" {
		return sourceFor(name, "")
	}
	return strings.TrimSuffix(strings.TrimPrefix(text, "git+"), ".git")
}

func addProvenance(repo string, components []legal.Component, entries []legal.Provenance, scope string) ([]legal.Component, error) {
	for _, entry := range entries {
		if !slices.Contains(entry.ArtifactScopes, scope) && !(scope == "server/browser" && slices.Contains(entry.ArtifactScopes, "server")) {
			continue
		}
		if entry.Name == "" || entry.Version == "" || entry.LicenseExpression == "" || strings.Contains(strings.ToUpper(entry.LicenseExpression), "UNKNOWN") || entry.ReviewNotes == "" {
			return nil, fmt.Errorf("provenance entry %q is incomplete or unresolved", entry.Name)
		}
		read := func(path, display, expected, label string) ([]byte, error) {
			data, err := os.ReadFile(path)
			if err != nil {
				return nil, fmt.Errorf("provenance %s %s %s: %w", entry.Name, label, display, err)
			}
			if expected != legal.SHA256(data) {
				return nil, fmt.Errorf("provenance %s %s hash changed: %s", entry.Name, label, display)
			}
			return data, nil
		}
		for _, artifact := range entry.LocalArtifacts {
			if artifact.Path == "" || artifact.SHA256 == "" {
				return nil, fmt.Errorf("provenance %s has an incomplete local artifact", entry.Name)
			}
			path := artifact.Path
			if !filepath.IsAbs(path) {
				path = filepath.Join(repo, filepath.FromSlash(path))
			}
			if _, err := read(path, artifact.Path, artifact.SHA256, "local artifact"); err != nil {
				return nil, err
			}
		}
		files := slices.Clone(entry.LocalLegalFiles)
		for i := range files {
			path := filepath.Join(repo, filepath.FromSlash(files[i].Name))
			data, err := read(path, files[i].Name, files[i].SHA256, "legal file")
			if err != nil {
				return nil, err
			}
			files[i].Text = string(data)
			if files[i].Kind == "" {
				files[i].Kind = "license"
			}
			// Provenance paths identify the checked-in source of a manual artifact.
			files[i].Name = filepath.Base(files[i].Name)
		}
		component := componentFromFiles(entry.Ecosystem, entry.Name, entry.Version, entry.Upstream, files)
		component.DeclaredLicenseExpression = entry.LicenseExpression
		component.SelectedLicenseExpression = entry.LicenseExpression
		component.Modified = entry.Modified
		components = append(components, component)
	}
	return sortComponents(components), nil
}

func sortComponents(components []legal.Component) []legal.Component {
	seen := map[string]legal.Component{}
	for _, component := range components {
		seen[componentKey(component)] = component
	}
	return slices.SortedFunc(maps.Values(seen), compareComponents)
}

func componentKey(component legal.Component) string {
	return component.Ecosystem + "\x00" + component.Name + "\x00" + component.Version
}

func compareComponents(a, b legal.Component) int {
	return cmp.Compare(componentKey(a), componentKey(b))
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
	for _, scope := range []componentScope{{"server", server}, {"tui", tui}, {"container", container}} {
		inv, err := marshal(inventory{SchemaVersion: 1, Scope: scope.name, Components: scope.components})
		if err != nil {
			return nil, err
		}
		base := filepath.Join("legal", "generated", scope.name)
		files = append(files,
			outputFile{filepath.Join(base, "inventory.json"), inv},
			outputFile{filepath.Join(base, "THIRD_PARTY_NOTICES.txt"), []byte(notices(scope.components))},
			outputFile{filepath.Join(base, "SOURCE.txt"), []byte(sourceURL + "\n")},
		)
	}
	webAbout := about{
		SchemaVersion: 2,
		Project:       project,
		SourceVersion: version,
		SourceURL:     sourceURL,
		LicenseURL:    "legal/LICENSE.txt",
		NoticesURL:    "legal/THIRD_PARTY_NOTICES.txt",
		Components:    make([]aboutComponent, 0, len(server)),
	}
	for _, component := range server {
		webAbout.Components = append(webAbout.Components, aboutComponent{
			Name: component.Name, Version: component.Version, Ecosystem: component.Ecosystem,
			Source: component.Source, DeclaredLicenseExpression: component.DeclaredLicenseExpression,
			SelectedLicenseExpression: component.SelectedLicenseExpression, Modified: component.Modified,
		})
	}
	web, err := marshal(webAbout)
	if err != nil {
		return nil, err
	}
	files = append(files, outputFile{"client/public/legal/about.json", web})
	files = append(files, outputFile{"client/public/legal/LICENSE.txt", licenseText})
	files = append(files, outputFile{"client/public/legal/THIRD_PARTY_NOTICES.txt", []byte(notices(server))})
	var tuiReport strings.Builder
	tuiReport.WriteString(copyText)
	fmt.Fprintf(&tuiReport, "\nSource code: %s\n\nLICENSE\n\n", sourceURL)
	tuiReport.Write(licenseText)
	if len(licenseText) == 0 || licenseText[len(licenseText)-1] != '\n' {
		tuiReport.WriteByte('\n')
	}
	tuiReport.WriteString("\n")
	tuiReport.WriteString(notices(tui))
	files = append(files, outputFile{filepath.Join("go", "internal", "legal", "assets", "TUI_LEGAL.txt"), []byte(tuiReport.String())})
	return files, nil
}

func marshal(value any) ([]byte, error) {
	// Generated legal artifacts have a checked-in indentation and trailing newline contract.
	b, err := json.Marshal(value, json.Deterministic(true), jsontext.WithIndent("  "))
	if err != nil {
		return nil, err
	}
	return append(b, '\n'), nil
}

func notices(components []legal.Component) string {
	var out strings.Builder
	out.WriteString("THIRD-PARTY SOFTWARE NOTICES\n\nGraphite Meter includes third-party software described below.\n\n")
	for _, component := range components {
		modified := "no"
		if component.Modified {
			modified = "yes"
		}
		fmt.Fprintf(&out, "Component: %s\nVersion: %s\nSource: %s\nLicense: %s\nModified by Graphite Meter: %s\n\n", component.Name, component.Version, component.Source, component.SelectedLicenseExpression, modified)
		for _, file := range slices.Concat(component.LegalTexts, component.Notices) {
			fmt.Fprintf(&out, "--- %s ---\n\n%s\n", file.Name, file.Text)
		}
		out.WriteString("============================================================\n\n")
	}
	return out.String()
}

func printReviewTemplate(server, tui, container []legal.Component) {
	seen := map[string]legal.Component{}
	for _, component := range slices.Concat(server, tui, container) {
		seen[component.Ecosystem+"\x00"+component.Name] = component
	}
	var out []map[string]any
	for _, component := range slices.SortedFunc(maps.Values(seen), compareComponents) {
		files := slices.Concat(component.LegalTexts, component.Notices)
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
	for _, scoped := range []componentScope{
		{"server/browser", server}, {"tui", tui}, {"container", container},
	} {
		for _, component := range scoped.components {
			matched := findReview(component.Ecosystem, component.Name, reviews)
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
