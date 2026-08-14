export interface LegalFile {
  name: string;
  sha256: string;
  text: string;
  kind?: string;
}

export interface LegalComponent {
  name: string;
  version: string;
  ecosystem: string;
  source: string;
  declaredLicenseExpression: string;
  selectedLicenseExpression: string;
  modified: boolean;
  legalTexts: LegalFile[];
  notices: LegalFile[];
}

export interface LegalProject {
  schemaVersion: number;
  name: string;
  copyrightHolder: string;
  copyrightYears: string;
  licenseExpression: string;
  repository: string;
}

export interface LegalAbout {
  schemaVersion: number;
  project: LegalProject;
  sourceVersion: string;
  sourceURL: string;
  license: string;
  components: LegalComponent[];
}
