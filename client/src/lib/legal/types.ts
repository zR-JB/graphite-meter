export interface LegalComponent {
  name: string;
  version: string;
  ecosystem: string;
  source: string;
  declaredLicenseExpression: string;
  selectedLicenseExpression: string;
  modified: boolean;
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
  licenseURL: string;
  noticesURL: string;
  components: LegalComponent[];
}
