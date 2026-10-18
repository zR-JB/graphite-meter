// Constants the build defines (build.ts).
declare const BUILD: {
  profile: "dev" | "prod";
  version: string | null;
  revision: string;
  clientVersion: string;
};
declare const WORKERS: { clock: string; lane: string; session: string; probe: string };
declare module "*.css";
