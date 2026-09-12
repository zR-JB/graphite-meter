import { configureFleet } from "./server-fleet-actions";
import { fleet } from "./multi-server-fixtures";
export { ready, savedResult } from "./server-fleet-actions";
export const configure = configureFleet.bind(null, fleet);
