package legal

import (
	"bytes"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"sort"
	"strings"
)

var candidatePrefixes = []string{"LICENSE", "LICENCE", "COPYING", "NOTICE", "COPYRIGHT", "PATENTS", "THIRD_PARTY"}

type Project struct {
	SchemaVersion     int    `json:"schemaVersion"`
	Name              string `json:"name"`
	CopyrightHolder   string `json:"copyrightHolder"`
	CopyrightYears    string `json:"copyrightYears"`
	LicenseExpression string `json:"licenseExpression"`
	Repository        string `json:"repository"`
}

type LegalFile struct {
	Name   string `json:"name"`
	SHA256 string `json:"sha256"`
	Text   string `json:"text,omitempty"`
	Kind   string `json:"kind,omitempty"`
}

type LocalArtifact struct {
	Path   string `json:"path"`
	SHA256 string `json:"sha256"`
}

type Review struct {
	Ecosystem                 string      `json:"ecosystem"`
	Name                      string      `json:"name"`
	ReviewedVersion           string      `json:"reviewedVersion"`
	Upstream                  string      `json:"upstream"`
	DeclaredLicenseExpression string      `json:"declaredLicenseExpression"`
	SelectedLicenseExpression string      `json:"selectedLicenseExpression"`
	LegalFiles                []LegalFile `json:"legalFiles"`
	Modified                  bool        `json:"modified"`
	ArtifactScopes            []string    `json:"artifactScopes,omitempty"`
	ReviewDecision            string      `json:"reviewDecision"`
	ReviewNotes               string      `json:"reviewNotes"`
}

type Provenance struct {
	Ecosystem           string          `json:"ecosystem"`
	Name                string          `json:"name"`
	Version             string          `json:"version"`
	Upstream            string          `json:"upstream"`
	UpstreamRevision    string          `json:"upstreamRevision"`
	LicenseExpression   string          `json:"licenseExpression"`
	Modified            bool            `json:"modified"`
	ModificationNote    string          `json:"modificationNote,omitempty"`
	ModificationDate    string          `json:"modificationDate,omitempty"`
	ArtifactScopes      []string        `json:"artifactScopes"`
	LocalPaths          []string        `json:"localPaths"`
	LocalArtifacts      []LocalArtifact `json:"localArtifacts,omitempty"`
	LocalLegalFiles     []LegalFile     `json:"localLegalFiles"`
	CorrespondingSource string          `json:"correspondingSource,omitempty"`
	ReviewNotes         string          `json:"reviewNotes"`
}

type Component struct {
	Name                      string      `json:"name"`
	Version                   string      `json:"version"`
	Ecosystem                 string      `json:"ecosystem"`
	Source                    string      `json:"source"`
	DeclaredLicenseExpression string      `json:"declaredLicenseExpression"`
	SelectedLicenseExpression string      `json:"selectedLicenseExpression"`
	Modified                  bool        `json:"modified"`
	LegalTexts                []LegalFile `json:"legalTexts"`
	Notices                   []LegalFile `json:"notices"`
	SourcePath                string      `json:"-"`
}

type GoPackage struct {
	ImportPath string
	Standard   bool
	Module     *struct {
		Path    string
		Version string
		Dir     string
		Replace *struct {
			Path    string
			Version string
			Dir     string
		}
	}
	Dir string
}

func ReadJSON[T any](path string, dst *T) error {
	b, err := os.ReadFile(path)
	if err != nil {
		return err
	}
	dec := json.NewDecoder(bytes.NewReader(b))
	if err := dec.Decode(dst); err != nil {
		return fmt.Errorf("decode %s: %w", path, err)
	}
	return nil
}

func ReadProject(repo string) (Project, error) {
	var p Project
	err := ReadJSON(filepath.Join(repo, "legal", "project.json"), &p)
	if err == nil && (p.Name == "" || p.Repository == "" || p.LicenseExpression == "") {
		err = errors.New("legal/project.json is incomplete")
	}
	return p, err
}

func ReadReviews(repo string) ([]Review, error) {
	var v []Review
	err := ReadJSON(filepath.Join(repo, "legal", "reviewed-components.json"), &v)
	return v, err
}

func ReadProvenance(repo string) ([]Provenance, error) {
	var v []Provenance
	err := ReadJSON(filepath.Join(repo, "legal", "provenance.json"), &v)
	return v, err
}

func SHA256(b []byte) string {
	sum := sha256.Sum256(b)
	return hex.EncodeToString(sum[:])
}

func isCandidate(name string) bool {
	upper := strings.ToUpper(name)
	for _, prefix := range candidatePrefixes {
		if upper == prefix || strings.HasPrefix(upper, prefix+".") || strings.HasPrefix(upper, prefix+"-") || strings.HasPrefix(upper, prefix+"_") {
			return true
		}
	}
	return false
}

func fileKind(name string) string {
	upper := strings.ToUpper(name)
	if strings.HasPrefix(upper, "NOTICE") || strings.HasPrefix(upper, "COPYRIGHT") || strings.HasPrefix(upper, "PATENTS") || strings.HasPrefix(upper, "THIRD_PARTY") {
		return "notice"
	}
	return "license"
}

func LegalFileKind(name string) string {
	return fileKind(filepath.Base(filepath.FromSlash(name)))
}

func ReadLegalFiles(dir string) ([]LegalFile, error) {
	files := make([]LegalFile, 0)
	err := filepath.WalkDir(dir, func(path string, entry os.DirEntry, walkErr error) error {
		if walkErr != nil {
			return walkErr
		}
		if entry.IsDir() || !isCandidate(entry.Name()) {
			return nil
		}
		b, err := os.ReadFile(path)
		if err != nil {
			return err
		}
		name, err := filepath.Rel(dir, path)
		if err != nil {
			return err
		}
		files = append(files, LegalFile{Name: filepath.ToSlash(name), SHA256: SHA256(b), Text: string(b), Kind: fileKind(entry.Name())})
		return nil
	})
	if err != nil {
		return nil, err
	}
	sort.Slice(files, func(i, j int) bool { return strings.ToLower(files[i].Name) < strings.ToLower(files[j].Name) })
	if len(files) == 0 {
		return nil, fmt.Errorf("no legal candidate file in %s", dir)
	}
	return files, nil
}

func ReadRootLegalFiles(dir string) ([]LegalFile, error) {
	entries, err := os.ReadDir(dir)
	if err != nil {
		return nil, err
	}
	files := make([]LegalFile, 0)
	for _, entry := range entries {
		if entry.IsDir() || !isCandidate(entry.Name()) {
			continue
		}
		b, err := os.ReadFile(filepath.Join(dir, entry.Name()))
		if err != nil {
			return nil, err
		}
		files = append(files, LegalFile{Name: entry.Name(), SHA256: SHA256(b), Text: string(b), Kind: fileKind(entry.Name())})
	}
	sort.Slice(files, func(i, j int) bool { return strings.ToLower(files[i].Name) < strings.ToLower(files[j].Name) })
	if len(files) == 0 {
		return nil, fmt.Errorf("no legal candidate file in %s", dir)
	}
	return files, nil
}

func Fingerprint(files []LegalFile) map[string]string {
	result := make(map[string]string, len(files))
	for _, file := range files {
		result[strings.ToLower(file.Name)] = file.SHA256
	}
	return result
}

func reviewMap(reviews []Review) map[string]Review {
	result := make(map[string]Review, len(reviews))
	for _, r := range reviews {
		result[r.Ecosystem+"\x00"+r.Name] = r
	}
	return result
}

func ValidateReview(c Component, reviews []Review) error {
	if c.DeclaredLicenseExpression == "" || strings.Contains(strings.ToUpper(c.DeclaredLicenseExpression), "UNKNOWN") || strings.Contains(strings.ToUpper(c.DeclaredLicenseExpression), "NOASSERTION") || strings.EqualFold(c.DeclaredLicenseExpression, "UNLICENSED") {
		return fmt.Errorf("component %s has missing or unknown licensing information", c.Name)
	}
	r, ok := reviewMap(reviews)[c.Ecosystem+"\x00"+c.Name]
	if !ok {
		return fmt.Errorf("LEGAL REVIEW REQUIRED: new component has no review record: %s", c.Name)
	}
	if r.ReviewDecision != "approved" {
		return fmt.Errorf("LEGAL REVIEW REQUIRED: review for %s is unresolved", c.Name)
	}
	if r.ReviewedVersion != "" && r.ReviewedVersion != c.Version {
		return fmt.Errorf("LEGAL REVIEW REQUIRED: reviewed version changed for %s", c.Name)
	}
	if r.DeclaredLicenseExpression != c.DeclaredLicenseExpression || r.SelectedLicenseExpression == "" {
		return fmt.Errorf("LEGAL REVIEW REQUIRED: declared license changed for %s", c.Name)
	}
	if c.Modified != r.Modified {
		return fmt.Errorf("LEGAL REVIEW REQUIRED: modification status changed for %s", c.Name)
	}
	currentFiles := append(append([]LegalFile{}, c.LegalTexts...), c.Notices...)
	if name, expected, actual, changed := fingerprintMismatch(currentFiles, r.LegalFiles); changed {
		return fmt.Errorf("LEGAL REVIEW REQUIRED: legal fingerprint changed for %s:\n  %s\n  expected: %s\n  actual:   %s", c.Name, name, expected, actual)
	}
	return nil
}

func fingerprintMismatch(current, reviewed []LegalFile) (name, expected, actual string, changed bool) {
	currentFingerprint := Fingerprint(current)
	reviewedFingerprint := Fingerprint(reviewed)
	names := make(map[string]struct{}, len(currentFingerprint)+len(reviewedFingerprint))
	displayNames := make(map[string]string, len(currentFingerprint)+len(reviewedFingerprint))
	for _, file := range current {
		key := strings.ToLower(file.Name)
		if _, ok := displayNames[key]; !ok {
			displayNames[key] = file.Name
		}
	}
	for _, file := range reviewed {
		key := strings.ToLower(file.Name)
		if _, ok := displayNames[key]; !ok {
			displayNames[key] = file.Name
		}
	}
	for name := range currentFingerprint {
		names[name] = struct{}{}
	}
	for name := range reviewedFingerprint {
		names[name] = struct{}{}
	}
	orderedNames := make([]string, 0, len(names))
	for name := range names {
		orderedNames = append(orderedNames, name)
	}
	sort.Strings(orderedNames)
	for _, name := range orderedNames {
		expectedHash, expectedOK := reviewedFingerprint[name]
		actualHash, actualOK := currentFingerprint[name]
		if !expectedOK || !actualOK || expectedHash != actualHash {
			if !expectedOK {
				expectedHash = "<missing>"
			}
			if !actualOK {
				actualHash = "<missing>"
			}
			return displayNames[name], expectedHash, actualHash, true
		}
	}
	return "", "", "", false
}

func RunGoList(repo, target, goos, goarch string) ([]GoPackage, error) {
	cmd := exec.Command("go", "list", "-deps", "-json", target)
	cmd.Dir = filepath.Join(repo, "go")
	cmd.Env = append(os.Environ(), "CGO_ENABLED=0", "GOOS="+goos, "GOARCH="+goarch)
	out, err := cmd.Output()
	if err != nil {
		return nil, fmt.Errorf("go list %s %s/%s: %w", target, goos, goarch, err)
	}
	dec := json.NewDecoder(bytes.NewReader(out))
	packages := make([]GoPackage, 0)
	for {
		var p GoPackage
		err := dec.Decode(&p)
		if errors.Is(err, io.EOF) {
			break
		}
		if err != nil {
			return nil, fmt.Errorf("decode go list: %w", err)
		}
		packages = append(packages, p)
	}
	return packages, nil
}
