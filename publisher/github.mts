export type GitHubRequest = (endpoint: string) => Promise<unknown>;

function record(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("Invalid GitHub API response");
  return value as Record<string, unknown>;
}

export function githubClient(token: string, fetcher: typeof fetch = fetch): GitHubRequest {
  return async (endpoint) => {
    if (!endpoint.startsWith("/repos/") || endpoint.startsWith("//")) throw new Error("Invalid GitHub API endpoint");
    const response = await fetcher(`https://api.github.com${endpoint}`, {
      headers: {
        Accept: "application/vnd.github+json",
        Authorization: `Bearer ${token}`,
        "X-GitHub-Api-Version": "2022-11-28",
      },
      redirect: "error",
      signal: AbortSignal.timeout(30_000),
    });
    if (!response.ok) {
      throw new Error(`GitHub API returned HTTP ${response.status}; check the token's repository and Contents/Actions permissions.`);
    }
    return response.json() as Promise<unknown>;
  };
}

export async function waitForDeployment(
  request: GitHubRequest,
  repository: string,
  revision: string,
  options: { timeoutMs?: number; intervalMs?: number; now?: () => number; sleep?: (ms: number) => Promise<void> } = {},
): Promise<void> {
  const now = options.now ?? Date.now;
  const sleep = options.sleep ?? ((ms) => new Promise((resolve) => setTimeout(resolve, ms)));
  const deadline = now() + (options.timeoutMs ?? 20 * 60_000);
  while (now() < deadline) {
    const result = record(await request(`/repos/${repository}/actions/workflows/pages.yml/runs?head_sha=${revision}&event=push&per_page=10`));
    if (!Array.isArray(result.workflow_runs)) throw new Error("GitHub did not return workflow runs");
    const run = result.workflow_runs.map(record).find((item) => item.head_sha === revision);
    if (run?.status === "completed") {
      if (run.conclusion !== "success") throw new Error(`Publication workflow did not succeed (conclusion=${String(run.conclusion)}).`);
      if (typeof run.id !== "number") throw new Error("GitHub workflow run has no numeric ID");
      const jobs = record(await request(`/repos/${repository}/actions/runs/${run.id}/jobs?filter=latest&per_page=100`));
      if (!Array.isArray(jobs.jobs)) throw new Error("GitHub did not return workflow jobs");
      const deploy = jobs.jobs.map(record).find((job) => job.name === "deploy");
      if (deploy?.conclusion !== "success") {
        throw new Error("Snapshot was pushed but NOT deployed. Check Pages settings, the main branch, and PAGES_DEPLOY_ENABLED.");
      }
      return;
    }
    await sleep(options.intervalMs ?? 10_000);
  }
  throw new Error("Timed out waiting for the snapshot's Pages deployment. Inspect GitHub Actions before retrying.");
}
