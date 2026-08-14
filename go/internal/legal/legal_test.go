package legal

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestReadLegalFilesPreservesCandidateBytesAndKinds(t *testing.T) {
	dir := t.TempDir()
	files := map[string]string{
		"license.MIT": "MIT License\n<script>literal</script>\n",
		"NOTICE.txt":  "Copyright upstream\n",
		"README.md":   "not a legal file\n",
	}
	for name, text := range files {
		if err := os.WriteFile(filepath.Join(dir, name), []byte(text), 0o644); err != nil {
			t.Fatal(err)
		}
	}
	got, err := ReadLegalFiles(dir)
	if err != nil {
		t.Fatal(err)
	}
	if len(got) != 2 || got[0].Name != "license.MIT" || got[1].Name != "NOTICE.txt" {
		t.Fatalf("legal files = %#v", got)
	}
	if got[0].Text != files["license.MIT"] || got[0].Kind != "license" {
		t.Fatalf("license was not preserved: %#v", got[0])
	}
	if got[1].Kind != "notice" {
		t.Fatalf("NOTICE kind = %q", got[1].Kind)
	}
	if !strings.Contains(got[0].Text, "<script>") {
		t.Fatal("license text was interpreted instead of preserved")
	}
}

func TestValidateReviewRequiresIdentityAndCompleteFingerprint(t *testing.T) {
	file := LegalFile{Name: "LICENSE", SHA256: SHA256([]byte("MIT"))}
	component := Component{
		Name: "example", Ecosystem: "go", DeclaredLicenseExpression: "MIT",
		SelectedLicenseExpression: "MIT", LegalTexts: []LegalFile{file},
	}
	review := Review{
		Name: "example", Ecosystem: "go", DeclaredLicenseExpression: "MIT",
		SelectedLicenseExpression: "MIT", LegalFiles: []LegalFile{file}, ReviewDecision: "approved",
	}
	if err := ValidateReview(component, []Review{review}); err != nil {
		t.Fatalf("identical reviewed component rejected: %v", err)
	}
	component.Version = "v2"
	if err := ValidateReview(component, []Review{review}); err != nil {
		t.Fatalf("version-only update rejected: %v", err)
	}
	component.LegalTexts[0].SHA256 = SHA256([]byte("changed"))
	if err := ValidateReview(component, []Review{review}); err == nil || !strings.Contains(err.Error(), "fingerprint") {
		t.Fatalf("changed fingerprint was not rejected: %v", err)
	}
}

func TestValidateReviewDoesNotTemplateApproveNewMITComponent(t *testing.T) {
	component := Component{Name: "new", Ecosystem: "npm", DeclaredLicenseExpression: "MIT"}
	err := ValidateReview(component, nil)
	if err == nil || !strings.Contains(err.Error(), "new component") {
		t.Fatalf("new component result = %v", err)
	}
}

func TestValidateReviewRejectsUnknownAndUnresolvedLicenses(t *testing.T) {
	for _, expression := range []string{"UNKNOWN", "NOASSERTION", "UNLICENSED"} {
		err := ValidateReview(Component{Name: expression, Ecosystem: "npm", DeclaredLicenseExpression: expression}, nil)
		if err == nil {
			t.Fatalf("%s was accepted", expression)
		}
	}
	component := Component{Name: "custom", Ecosystem: "npm", DeclaredLicenseExpression: "LicenseRef-Custom"}
	err := ValidateReview(component, []Review{{Name: "custom", Ecosystem: "npm", DeclaredLicenseExpression: "LicenseRef-Custom", SelectedLicenseExpression: "LicenseRef-Custom", ReviewDecision: "pending"}})
	if err == nil || !strings.Contains(err.Error(), "unresolved") {
		t.Fatalf("pending custom license result = %v", err)
	}
}

func TestValidateReviewRejectsNewNoticeAndChangedModification(t *testing.T) {
	license := LegalFile{Name: "LICENSE", SHA256: SHA256([]byte("MIT")), Kind: "license"}
	base := Component{
		Name: "example", Ecosystem: "go", DeclaredLicenseExpression: "MIT",
		SelectedLicenseExpression: "MIT", LegalTexts: []LegalFile{license},
	}
	review := Review{
		Name: "example", Ecosystem: "go", DeclaredLicenseExpression: "MIT",
		SelectedLicenseExpression: "MIT", LegalFiles: []LegalFile{license}, ReviewDecision: "approved",
	}
	withNotice := base
	withNotice.Notices = []LegalFile{{Name: "NOTICE", SHA256: SHA256([]byte("notice")), Kind: "notice"}}
	if err := ValidateReview(withNotice, []Review{review}); err == nil || !strings.Contains(err.Error(), "fingerprint") {
		t.Fatalf("new NOTICE was not rejected: %v", err)
	}
	modified := base
	modified.Modified = true
	if err := ValidateReview(modified, []Review{review}); err == nil || !strings.Contains(err.Error(), "modification") {
		t.Fatalf("changed modification status was not rejected: %v", err)
	}
}

func TestReadLegalFilesFailsWithoutCandidate(t *testing.T) {
	dir := t.TempDir()
	if err := os.WriteFile(filepath.Join(dir, "README.md"), []byte("not a license"), 0o644); err != nil {
		t.Fatal(err)
	}
	if _, err := ReadLegalFiles(dir); err == nil || !strings.Contains(err.Error(), "no legal candidate") {
		t.Fatalf("missing legal material result = %v", err)
	}
}
