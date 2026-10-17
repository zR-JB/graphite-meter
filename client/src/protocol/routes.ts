// Lists every measurement route the browser reaches, as api/routes.txt pins them.
export const ROUTES = {
  servers: "/servers",
  preflight: "/preflight",
  probe: "/probe",
  download: "/download",
  upload: "/upload",
  uploadSession: "/upload/session",
  uploadProgress: "/upload/progress",
  uploadCheckpoint: "/upload/checkpoint",
  wtSession: "/wt/session",
  wsSession: "/ws/session",
  ping: "/ws/ping",
  wtDownload: "/wt/download",
  wtUpload: "/wt/upload",
  wtPing: "/wt/ping",
} as const;

/** The sign-in routes, which the measurement route pin leaves out. */
export const AUTH_ROUTES = {
  session: "/auth/session",
  browser: "/auth/browser",
  browserToken: "/auth/browser/token",
  login: "/login",
} as const;
