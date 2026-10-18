// About & legal: the project's license and every third-party component the server ships.
import "./legal.css";
import { Show } from "../../lib/dom";
import { effect, signal, untrack, type Read } from "../../lib/signal";
import { Dialog } from "../kit/dialog";
import { Icon } from "../kit/icon";
import { loadLegal, type About, type LegalComponent } from "./loader";

const ECOSYSTEM: Record<string, string> = {
  cargo: "Rust crates",
  npm: "npm packages",
  font: "Fonts",
};

/** Names a location as people know it, a repository or crate by its first two path parts and any other page by its
    host and path. Revisions, compare ranges and versions stay in the link. */
function place(url: string): string {
  const { host, pathname } = new URL(url, document.baseURI);
  const parts = pathname.split("/").filter(Boolean);
  return [host, ...(host === "github.com" || host === "crates.io" ? parts.slice(0, 2) : parts)].join("/");
}

const Out = (props: { href: string; text: string; name: string }) => (
  <a href={props.href} target="_blank" rel="noopener noreferrer">
    <span>{props.text}</span>
    <span class="sr-only">{props.name}</span>
    <Icon name="external" />
  </a>
);

// Every component reads one way. It shows where it comes from (a fork's upstream), then, when modified, the shipped
// copy and what changed.
function Component({ component }: { component: LegalComponent }): Node {
  const url = (label: string) => component.links.find((link) => link.label === label)?.url;
  const [source, upstream, changes] = [url("Source"), url("Upstream"), url("Changes")];
  const origin = upstream ?? source;
  const fork = upstream ? source : undefined;
  return (
    <div class="component">
      <dt>{component.name}</dt>
      <dd>
        <span class="version">{component.version}</span>
        <span>{component.selectedLicenseExpression}</span>
        <span class="where">
          {origin && <Out href={origin} text={place(origin)} name={`, source of ${component.name}`} />}
          {component.modified && (
            <span class="modified">
              <span class="aside">Modified</span>
              {fork && <Out href={fork} text={place(fork)} name={`, shipped source of ${component.name}`} />}
              {changes && <Out href={changes} text="changes" name={` to ${component.name}`} />}
            </span>
          )}
        </span>
      </dd>
    </div>
  );
}

function Notices({ about }: { about: About }): Node {
  const { project } = about;
  return (
    <>
      <section class="group" aria-labelledby="project-legal-title">
        <h3 id="project-legal-title">{project.name}</h3>
        <dl class="kv">
          <div>
            <dt>Copyright</dt>
            <dd>
              © {project.copyrightYears} {project.copyrightHolder}
            </dd>
          </div>
          <div>
            <dt>License</dt>
            <dd>{project.licenseExpression}</dd>
          </div>
          <div>
            <dt>Warranty</dt>
            <dd>
              Graphite Meter is free software. It comes with absolutely no warranty, to the extent permitted by
              applicable law.
            </dd>
          </div>
        </dl>
        <div class="kv">
          {(
            [
              ["Source code", about.sourceURL],
              ["Project license", about.licenseURL],
              ["Third-party notices", about.noticesURL],
            ] as const
          ).map(([label, href]) => (
            <a class="link-row" href={href} target="_blank" rel="noopener noreferrer">
              {label}
              <Icon name="external" />
            </a>
          ))}
        </div>
      </section>
      <section class="third-party" aria-labelledby="third-party-title">
        <div class="group-head">
          <h3 id="third-party-title">Third-party software</h3>
        </div>
        {[...Map.groupBy(about.components, (component) => component.ecosystem)].map(([ecosystem, components]) => (
          <section class="group" aria-label={ECOSYSTEM[ecosystem] ?? ecosystem}>
            <h4 class="list-label">{ECOSYSTEM[ecosystem] ?? ecosystem}</h4>
            <dl class="kv components">
              {components.map((component) => (
                <Component component={component} />
              ))}
            </dl>
          </section>
        ))}
      </section>
    </>
  );
}

export function LegalDialog(props: { open: Read<boolean>; invoker: () => HTMLElement | null; onClose(): void }): Node {
  const about = signal<About | null>(null);
  const failed = signal(false);
  let loading = false;
  const load = async () => {
    if (loading) return;
    loading = true;
    failed.set(false);
    try {
      about.set(await loadLegal());
    } catch {
      failed.set(true);
    } finally {
      loading = false;
    }
  };
  // A first open loads the notices, and so does an open after a failure.
  effect(() => {
    if (props.open() && !untrack(about)) void untrack(load);
  });
  const title = (
    <h2 id="legal-dialog-title" tabindex="-1" autofocus="">
      About &amp; legal
    </h2>
  ) as HTMLElement;
  // Retry leaves with its button, so focus moves to the title first and stays in the dialog.
  const retry = () => {
    title.focus({ preventScroll: true });
    void load();
  };
  return (
    <Dialog
      open={props.open}
      invoker={props.invoker}
      onCancel={props.onClose}
      class="float sheet legal-dialog"
      labelledby="legal-dialog-title"
      lightDismiss
    >
      <header class="sheet-head">
        {title}
        <div class="head-actions">
          <button class="btn btn-icon btn-quiet" type="button" aria-label="Close About & legal" onclick={props.onClose}>
            <Icon name="close" />
          </button>
        </div>
      </header>
      <div class="sheet-body legal-body">
        {() =>
          about() ? null : failed() ? (
            <div class="legal-status" role="alert">
              <p>Could not load legal notices.</p>
              <button class="btn" type="button" onclick={retry}>
                Retry
              </button>
            </div>
          ) : (
            <p class="legal-status" role="status">
              Loading legal notices…
            </p>
          )
        }
        <Show when={about}>{(data) => <Notices about={data()} />}</Show>
      </div>
    </Dialog>
  );
}
