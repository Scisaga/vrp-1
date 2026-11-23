"""Record local Workbench browser QA for the two plans returned by the MCP."""
from urllib.parse import urlsplit

from playwright.sync_api import expect, sync_playwright


def assert_schedule_layout(page):
    expect(page.locator("#gantt-scroll")).to_be_visible(timeout=25_000)
    issues = page.evaluate("""() => {
        const panel = document.querySelector('.schedule-panel');
        if (!panel) return ['Schedule panel is missing'];
        const bounds = selector => panel.querySelector(selector).getBoundingClientRect();
        const head = bounds('.schedule-head');
        const legend = bounds('.legend');
        const gantt = bounds('#gantt-scroll');
        const footer = bounds('.schedule-footer');
        const outside = (item, row) => {
            const r = item.getBoundingClientRect();
            return r.top < row.top - 1 || r.bottom > row.bottom + 1 ||
                   r.left < row.left - 1 || r.right > row.right + 1;
        };
        const issues = [];
        if (head.bottom > legend.top + 1 || legend.bottom > gantt.top + 1)
            issues.push('Schedule toolbar, legend and timeline are not on separate rows');
        if (gantt.height === 0 || footer.bottom > panel.getBoundingClientRect().bottom + 1)
            issues.push(`Schedule is clipped at ${innerWidth}px: timeline ${gantt.height}px, ` +
                        `footer ends ${footer.bottom - panel.getBoundingClientRect().bottom}px below panel`);
        for (const node of panel.querySelectorAll('.schedule-head button, .schedule-head select, .schedule-head input'))
            if (outside(node, head)) issues.push(`Toolbar control does not fit: ${node.textContent || node.id}`);
        for (const node of panel.querySelectorAll('.legend > span'))
            if (outside(node, legend)) issues.push(`Legend item does not fit: ${node.textContent}`);
        return issues;
    }""")
    assert not issues, issues


def capture(base, evidence, output, *, token):
    output.mkdir(parents=True, exist_ok=False)
    with sync_playwright() as playwright:
        browser = playwright.chromium.launch()
        context = browser.new_context(
            viewport={"width": 1080, "height": 900},
            device_scale_factor=1,
            # Exercise remote authentication against the local test server.
            extra_http_headers={"X-Forwarded-For": "192.0.2.1"},
            record_video_dir=output,
            record_video_size={"width": 1080, "height": 900},
        )
        page = context.new_page()
        errors = []
        requests = []
        page.on("pageerror", lambda error: errors.append(str(error)))
        page.on("request", lambda request: requests.append(urlsplit(request.url).path))
        for run in evidence["runs"]:
            plan = run["plan"]
            path = urlsplit(plan["viewUrl"])
            url = base + path.path + ("?" + path.query if path.query else "") + "#" + path.fragment
            # Start as a fresh WorkBuddy tab with no in-memory workbench token.
            page.goto("about:blank")
            with page.expect_response(f"{base}/healthz") as health:
                page.goto(url, wait_until="domcontentloaded")
            assert health.value.status == 401, "The workbench must request authentication"
            expect(page.locator(".app-header")).to_be_hidden()
            expect(page.locator("#sidebar")).to_be_hidden()
            page.get_by_role("button", name="输入访问令牌").click()
            page.locator('input[name="token"]').fill(token)
            page.get_by_role("button", name="连接服务").click()
            expect(page.locator("#gantt-scroll")).to_be_visible(timeout=30_000)
            assert_schedule_layout(page)
            page.set_viewport_size({"width": 500, "height": 600})
            assert_schedule_layout(page)
            page.set_viewport_size({"width": 1080, "height": 900})
            expect(page.locator("#scene-title")).to_be_visible()
            expect(page.locator("#workspace-tabs")).to_be_hidden()
            expect(page.locator("#global-content")).to_be_hidden()
            map_canvas = page.locator(".map-frame.interactive canvas").first
            expect(map_canvas).to_be_visible(timeout=45_000)
            expect(page.locator(".offline-basemap-status")).to_contain_text("离线底图可用", timeout=45_000)
            page.locator("#gantt-scroll").scroll_into_view_if_needed()
            expect(map_canvas).to_be_in_viewport(ratio=0.2)
            assert map_canvas.bounding_box()["width"] >= 300, "Map is too narrow to use in the split view"
            if run["phase"] == "replan":
                expect(page.locator("#workspace-content .banner").first).to_contain_text(f"候选 #{plan['jobId']}")
                expect(page.locator("#compare")).to_be_checked()
                page.locator('[data-action="show-published"]').click()
                expect(page.locator("#workspace-content .banner").first).to_contain_text("生效计划")
                expect(map_canvas).to_be_in_viewport(ratio=0.2)
                page.screenshot(path=output / "published.png")
                page.wait_for_timeout(1000)
                page.locator('[data-action="show-candidate"]').click()
                expect(page.locator("#workspace-content .banner").first).to_contain_text(f"候选 #{plan['jobId']}")
                expect(page).to_have_url(url)
                page.screenshot(path=output / "replan-compare.png")
                page.wait_for_timeout(1000)
                ticket = plan["ticket"]["ticketId"]
                active = page.locator(f'#gantt-scroll [data-select-ticket="{ticket}"]').first
                active.scroll_into_view_if_needed()
                active.press("Enter")
                expect(page.locator(f'#gantt-scroll [data-select-ticket="{ticket}"]').first).to_have_attribute("aria-pressed", "true")
                expect(map_canvas).to_be_in_viewport(ratio=0.2)
            else:
                expect(page.locator(".progress-panel")).to_have_attribute("data-progress-job", plan["jobId"])
                page.locator('[data-action="clear-selection"]').click()
                expect(page.locator(".inspector-header h3")).to_have_text("全局路线")
            page.wait_for_timeout(800)
            page.screenshot(path=output / f"{run['phase']}.png")
            page.wait_for_timeout(1000)
        page.evaluate("location.hash = 'scenes'")
        expect(page.get_by_role("heading", name="无法打开场景方案")).to_be_visible()
        expect(page.locator("#scene-content")).to_be_hidden()
        expect(page.locator(".app-header")).to_be_hidden()
        assert "/v1/scenarios" not in requests, "The result panel queried the global scene list"

        page.goto(base + "/", wait_until="domcontentloaded")
        expect(page.locator(".app-header")).to_be_visible()
        expect(page.locator("#sidebar")).to_be_visible()
        if errors:
            raise AssertionError(errors)
        video = page.video
        context.close()
        video.save_as(output / "workbench-local-qa.webm")
        video.delete()
        browser.close()
