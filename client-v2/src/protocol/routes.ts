// Every server route the browser reaches (api/routes.txt pins the table across Go, Rust and the browser).
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
