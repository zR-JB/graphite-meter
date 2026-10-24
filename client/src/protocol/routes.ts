// Every measurement route the browser reaches, as api/routes.txt pins them.
export const ROUTES = {
  servers: "/servers",
  preflight: "/preflight",
  probe: "/probe",
  download: "/download",
  upload: "/upload",
  uploadProgress: "/upload/progress",
  uploadCheckpoint: "/upload/checkpoint",
  lease: "/lease",
  ping: "/ws/ping",
  wsProgress: "/ws/progress",
  wtDownload: "/wt/download",
  wtUpload: "/wt/upload",
  wtPing: "/wt/ping",
} as const;

export const AUTH_ROUTES = {
  login: "/login",
  password: "/auth/password",
  session: "/auth/session",
  browser: "/auth/browser",
  cliApprove: "/auth/cli/approve",
  browserApprove: "/auth/browser/approve",
  browserToken: "/auth/browser/token",
  logout: "/auth/logout",
} as const;
