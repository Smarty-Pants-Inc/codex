#!/usr/bin/env python3

import contextlib
import json
import os
import signal
import subprocess
import sys
import textwrap
import time
import unittest
from pathlib import Path
from tempfile import TemporaryDirectory

import run_bazel_with_buildbuddy


class RunBazelWithBuildBuddyTest(unittest.TestCase):
    def github_env(
        self,
        temp_dir: str,
        *,
        repository: str = "openai/codex",
        fork: bool = False,
        event_name: str = "pull_request",
    ) -> dict[str, str]:
        event_path = Path(temp_dir) / "event.json"
        event_path.write_text(
            json.dumps({"pull_request": {"head": {"repo": {"fork": fork}}}}),
            encoding="utf-8",
        )
        return {
            "BUILDBUDDY_API_KEY": "token",
            "GITHUB_ACTIONS": "true",
            "GITHUB_EVENT_NAME": event_name,
            "GITHUB_EVENT_PATH": str(event_path),
            "GITHUB_REPOSITORY": repository,
        }

    def test_keyless_invocation_drops_remote_ci_configuration(self) -> None:
        self.assertIsNone(
            run_bazel_with_buildbuddy.remote_config(
                ["build", "--config=ci-linux", "//codex-rs/cli:codex"],
                {},
            )
        )
        self.assertEqual(
            run_bazel_with_buildbuddy.bazel_args_with_remote_config(
                ["build", "--config=ci-linux", "--", "//codex-rs/cli:codex"],
                {},
            ),
            ["build", "--", "//codex-rs/cli:codex"],
        )

    def test_program_arguments_after_separator_do_not_select_or_lose_rbe(self) -> None:
        args = ["run", "//codex-rs/cli:codex", "--", "--config=remote"]

        self.assertEqual(
            run_bazel_with_buildbuddy.bazel_args_with_remote_config(args, {}),
            args,
        )
        self.assertEqual(
            run_bazel_with_buildbuddy.remote_config(
                args, {"BUILDBUDDY_API_KEY": "fork-token"}
            ),
            "buildbuddy-generic",
        )

    def test_upstream_push_selects_openai_rbe_before_target_separator(self) -> None:
        with TemporaryDirectory() as temp_dir:
            env = self.github_env(temp_dir, event_name="push")

            self.assertEqual(
                run_bazel_with_buildbuddy.bazel_args_with_remote_config(
                    ["build", "--config=ci-linux", "--", "//codex-rs/cli:codex"],
                    env,
                ),
                [
                    "build",
                    "--config=buildbuddy-openai-rbe",
                    "--remote_header=x-buildbuddy-api-key=token",
                    "--config=ci-linux",
                    "--",
                    "//codex-rs/cli:codex",
                ],
            )

    def test_windows_cross_ci_configuration_follows_remote_configuration(self) -> None:
        env = {"BUILDBUDDY_API_KEY": "fork-token"}

        self.assertEqual(
            run_bazel_with_buildbuddy.bazel_args_with_remote_config(
                ["build", "--config=ci-windows-cross", "//codex-rs/cli:codex"],
                env,
            ),
            [
                "build",
                "--config=buildbuddy-generic-rbe",
                "--remote_header=x-buildbuddy-api-key=fork-token",
                "--config=ci-windows-cross",
                "//codex-rs/cli:codex",
            ],
        )

    def test_query_remote_configuration_is_inserted_before_expression(self) -> None:
        expression = 'kind("rust_library rule", //codex-rs/...)'
        env = {"BUILDBUDDY_API_KEY": "fork-token"}

        for command in ("query", "cquery", "aquery"):
            with self.subTest(command=command):
                self.assertEqual(
                    run_bazel_with_buildbuddy.bazel_args_with_remote_config(
                        [
                            command,
                            "--config=ci-windows-cross",
                            "--output=label",
                            expression,
                        ],
                        env,
                    ),
                    [
                        command,
                        "--config=buildbuddy-generic-rbe",
                        "--remote_header=x-buildbuddy-api-key=fork-token",
                        "--config=ci-windows-cross",
                        "--output=label",
                        expression,
                    ],
                )

    def test_same_repository_pull_request_selects_openai_host(self) -> None:
        with TemporaryDirectory() as temp_dir:
            self.assertEqual(
                run_bazel_with_buildbuddy.remote_config(
                    ["build", "--config=ci-v8"], self.github_env(temp_dir)
                ),
                "buildbuddy-openai-rbe",
            )

    def test_fork_pull_request_cannot_select_openai_host(self) -> None:
        with TemporaryDirectory() as temp_dir:
            env = self.github_env(temp_dir, fork=True)

            self.assertEqual(
                run_bazel_with_buildbuddy.remote_config(
                    ["build", "--config=ci-v8"], env
                ),
                "buildbuddy-generic-rbe",
            )

    def test_run_in_fork_repository_cannot_select_openai_host(self) -> None:
        with TemporaryDirectory() as temp_dir:
            env = self.github_env(temp_dir, repository="contributor/codex")

            self.assertEqual(
                run_bazel_with_buildbuddy.remote_config(
                    ["build", "--config=ci-v8"], env
                ),
                "buildbuddy-generic-rbe",
            )

    def test_pull_request_without_readable_event_payload_fails_closed(self) -> None:
        for event_path in (None, "missing-event.json"):
            env = {
                "BUILDBUDDY_API_KEY": "token",
                "GITHUB_ACTIONS": "true",
                "GITHUB_EVENT_NAME": "pull_request",
                "GITHUB_REPOSITORY": "openai/codex",
            }
            if event_path is not None:
                env["GITHUB_EVENT_PATH"] = event_path

            with self.subTest(event_path=event_path):
                self.assertEqual(
                    run_bazel_with_buildbuddy.remote_config(["build"], env),
                    "buildbuddy-generic",
                )

    def test_bazel_command_uses_configured_binary_locally(self) -> None:
        self.assertEqual(
            run_bazel_with_buildbuddy.bazel_command(
                "info",
                "execution_root",
                env={"CODEX_BAZEL_BIN": "fake-bazel"},
            ),
            ["fake-bazel", "info", "execution_root"],
        )

    def test_bazel_command_normalizes_github_actions_startup_options(self) -> None:
        env = {
            "BAZEL_OUTPUT_USER_ROOT": "/tmp/bazel-output",
            "GITHUB_ACTIONS": "true",
        }

        self.assertEqual(
            run_bazel_with_buildbuddy.bazel_command("build", "//codex-rs/...", env=env),
            [
                "bazel",
                "--output_user_root=/tmp/bazel-output",
                "--noexperimental_remote_repo_contents_cache",
                "build",
                "//codex-rs/...",
            ],
        )
        self.assertEqual(
            run_bazel_with_buildbuddy.bazel_command(
                "--experimental_remote_repo_contents_cache",
                "build",
                "//codex-rs/...",
                env=env,
            ),
            [
                "bazel",
                "--output_user_root=/tmp/bazel-output",
                "--experimental_remote_repo_contents_cache",
                "build",
                "//codex-rs/...",
            ],
        )

    def test_bazel_command_uses_configured_local_caches(self) -> None:
        env = {
            "BAZEL_REPO_CONTENTS_CACHE": "/tmp/bazel-repo-contents",
            "BAZEL_REPOSITORY_CACHE": "/tmp/bazel-repository",
        }

        self.assertEqual(
            run_bazel_with_buildbuddy.bazel_command(
                "build",
                "--config=local",
                "//codex-rs/...",
                env=env,
            ),
            [
                "bazel",
                "build",
                "--config=local",
                "//codex-rs/...",
                "--repo_contents_cache=/tmp/bazel-repo-contents",
                "--repository_cache=/tmp/bazel-repository",
            ],
        )

    def test_bazel_command_adds_local_caches_before_separator(self) -> None:
        self.assertEqual(
            run_bazel_with_buildbuddy.bazel_command(
                "build",
                "//codex-rs/...",
                "--",
                "--program-arg",
                env={"BAZEL_REPOSITORY_CACHE": "/tmp/bazel-repository"},
            ),
            [
                "bazel",
                "build",
                "//codex-rs/...",
                "--repository_cache=/tmp/bazel-repository",
                "--",
                "--program-arg",
            ],
        )

    def test_main_preserves_spaced_argument_and_child_exit_status(self) -> None:
        spaced_arg = (
            r"--test_env=PATH=C:\Program Files\PowerShell\7;C:\Program Files\Git\bin"
        )
        child_code = (
            f"import sys; sys.exit(37 if sys.argv[1] == {spaced_arg!r} else 91)"
        )
        env = os.environ.copy()
        env["CODEX_BAZEL_BIN"] = sys.executable
        env.pop("BAZEL_OUTPUT_USER_ROOT", None)
        env.pop("BUILDBUDDY_API_KEY", None)
        env.pop("GITHUB_ACTIONS", None)

        result = subprocess.run(
            [
                sys.executable,
                str(Path(run_bazel_with_buildbuddy.__file__)),
                "-c",
                child_code,
                spaced_arg,
            ],
            env=env,
            check=False,
            capture_output=True,
            text=True,
        )

        self.assertEqual(result.returncode, 37, result.stderr)


@unittest.skipUnless(os.name == "posix", "POSIX client cancellation contract")
class RunBazelCiCancellationTest(unittest.TestCase):
    @contextlib.contextmanager
    def client(self, mode):
        with TemporaryDirectory() as directory:
            root = Path(directory)
            fake = root / "owned-client"
            fake.write_text(
                f"#!{sys.executable}\n"
                + textwrap.dedent("""\
                    import json, os, signal, subprocess, sys, time
                    from pathlib import Path
                    root = Path(os.environ['FIXTURE_ROOT'])
                    mode = os.environ['FIXTURE_MODE']
                    if '--fixture-child' not in sys.argv:
                        # Bazelisk1.28.1 test/build starts the client and ignores
                        # terminal signals while waiting, rather than execing it.
                        signal.signal(signal.SIGINT, signal.SIG_IGN)
                        signal.signal(signal.SIGTERM, signal.SIG_IGN)
                        if mode == 'launch':
                            (root / 'launching').touch()
                            deadline = time.monotonic() + 10
                            while not (root / 'start').exists():
                                if time.monotonic() >= deadline:
                                    sys.exit(99)
                                time.sleep(0.01)
                        child = subprocess.Popen([sys.executable, __file__,
                                                  '--fixture-child', *sys.argv[1:]])
                        status = child.wait()
                        (root / 'leader_joined').touch()
                        sys.exit(status)
                    interrupted = False
                    def interrupt(signum, frame):
                        global interrupted
                        interrupted = True
                        with (root / 'signals').open('a') as out:
                            out.write(str(signum) + '\\n')
                    signal.signal(signal.SIGINT, interrupt)
                    (root / 'ready.tmp').write_text(json.dumps({
                        'pid': os.getpid(), 'parent': os.getppid(),
                        'group': os.getpgrp(), 'args': sys.argv[2:]}))
                    (root / 'ready.tmp').replace(root / 'ready')
                    print('client stdout', flush=True)
                    print('client stderr', file=sys.stderr, flush=True)
                    if mode in ('cancel', 'unresponsive', 'launch'):
                        deadline = time.monotonic() + 10
                        while not (root / 'release').exists():
                            if mode in ('cancel', 'launch') and interrupted:
                                time.sleep(0.1)
                                break
                            if time.monotonic() >= deadline:
                                sys.exit(99)
                            time.sleep(0.01)
                    print('client final log', flush=True)
                    if mode == 'failure':
                        print('ERROR: fixture action failed:', flush=True)
                        sys.exit(37)
                    sys.exit(0)
                    """),
                encoding="utf-8",
            )
            fake.chmod(0o700)
            env = {key: os.environ[key] for key in ("PATH", "HOME", "TMPDIR") if key in os.environ}
            env.update(
                CODEX_BAZEL_BIN=str(fake),
                RUNNER_OS="macOS",
                FIXTURE_ROOT=str(root),
                FIXTURE_MODE=mode,
            )
            if mode == 'logger':
                logger = root / 'tee'
                logger.write_text(
                    f'#!{sys.executable}\n' + textwrap.dedent("""\
                        import os, sys, time
                        from pathlib import Path
                        root = Path(os.environ['FIXTURE_ROOT'])
                        with open(sys.argv[1], 'w') as log:
                            for line in sys.stdin:
                                log.write(line)
                                print(line, end='', flush=True)
                        (root / 'logger_eof').touch()
                        deadline = time.monotonic() + 10
                        while not (root / 'release').exists():
                            if time.monotonic() >= deadline:
                                sys.exit(99)
                            time.sleep(0.01)
                        """), encoding='utf-8')
                logger.chmod(0o700)
                env['PATH'] = str(root) + os.pathsep + env['PATH']
            script = Path(__file__).with_name("run-bazel-ci.sh").resolve()
            process = subprocess.Popen(
                ["bash", "-c", 'exec "$@"', "fixture", str(script),
                 "--print-failed-action-summary", "--", "test",
                 "--test_env=VALUE=with spaces", "--", "//fixture:target"],
                env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
            )
            try:
                yield process, root
            finally:
                # Only this fixture owns these children; release even an intentionally
                # unresponsive client before joining. No Bazel server is launched.
                (root / "release").touch()
                try:
                    process.communicate(timeout=12)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.communicate(timeout=2)

    def await_file(self, path, process):
        deadline = time.monotonic() + 5
        while not path.exists():
            self.assertIsNone(process.poll(), "owner exited before fixture readiness")
            self.assertLess(time.monotonic(), deadline, "fixture readiness timed out")
            time.sleep(0.01)
        return path.read_text()

    def test_success_and_failure_preserve_arguments_logs_and_status(self):
        for mode, status in (("success", 0), ("failure", 37)):
            with self.subTest(mode=mode), self.client(mode) as (process, root):
                stdout, stderr = process.communicate(timeout=5)
                self.assertEqual(process.returncode, status, stderr)
                ready = json.loads((root / "ready").read_text())
                self.assertEqual(ready['group'], ready['parent'])
                self.assertNotEqual(ready['group'], os.getpgrp())
                self.assertTrue((root / 'leader_joined').exists())
                self.assertEqual(ready['args'], [
                    '--noexperimental_remote_repo_contents_cache', 'test',
                    '--test_env=VALUE=with spaces', '--', '//fixture:target'])
                for line in ('client stdout', 'client stderr', 'client final log'):
                    self.assertIn(line, stdout)
                if mode == 'failure':
                    self.assertIn('Bazel failed action diagnostics:', stdout)

    def test_cancel_reaches_original_client_and_is_not_success(self):
        with self.client('cancel') as (process, root):
            ready = json.loads(self.await_file(root / 'ready', process))
            self.assertEqual(ready['group'], ready['parent'])
            self.assertNotEqual(ready['group'], os.getpgid(process.pid))
            process.send_signal(signal.SIGINT)
            stdout, stderr = process.communicate(timeout=5)
            self.assertEqual(process.returncode, 130, stderr)
            self.assertEqual((root / 'signals').read_text(), f'{signal.SIGINT}\n')
            self.assertIn(f'owned Bazel job group {ready["group"]}', stderr)
            self.assertTrue((root / 'leader_joined').exists())
            self.assertIn('client final log', stdout)
            self.assertNotIn('Bazel failed action diagnostics:', stdout)

    def test_cancellation_before_shim_launch_is_retained_not_false_success(self):
        with self.client('launch') as (process, root):
            self.await_file(root / 'launching', process)
            process.send_signal(signal.SIGTERM)
            time.sleep(0.05)
            self.assertIsNone(process.poll())
            (root / 'start').touch()
            self.await_file(root / 'ready', process)
            # A shim that has not forked its client can ignore the one signal.
            # We must retain cancellation and ownership, never invent quiescence
            # or escalate to a second signal or an unrelated process.
            self.assertIsNone(process.poll())
            (root / 'release').touch()
            stdout, stderr = process.communicate(timeout=5)
            self.assertEqual(process.returncode, 130, stderr)
            self.assertIn('client final log', stdout)
            self.assertTrue((root / 'leader_joined').exists())
            self.assertEqual(stderr.count('Cancelling owned Bazel job group'), 1)

    def test_owner_joins_logger_after_client_exits(self):
        with self.client('logger') as (process, root):
            self.await_file(root / 'logger_eof', process)
            self.assertIsNone(process.poll(), 'owner must join its logger')
            (root / 'release').touch()
            stdout, stderr = process.communicate(timeout=5)
            self.assertEqual(process.returncode, 0, stderr)
            self.assertIn('client final log', stdout)

    def test_unresponsive_client_keeps_owner_until_join_without_escalation(self):
        with self.client('unresponsive') as (process, root):
            ready = json.loads(self.await_file(root / 'ready', process))
            sentinel = subprocess.Popen([
                sys.executable, '-c',
                'import signal,time; signal.signal(signal.SIGINT, lambda *args: exit(77)); time.sleep(10)',
            ], start_new_session=True)
            try:
                self.assertNotEqual(ready['group'], os.getpgid(sentinel.pid))
                self.assertNotEqual(ready['group'], os.getpgid(process.pid))
                self.assertNotEqual(ready['group'], os.getpgrp())
                process.send_signal(signal.SIGTERM)
                self.await_file(root / 'signals', process)
                self.assertIsNone(sentinel.poll(), 'unrelated sentinel received cancellation')
            finally:
                sentinel.terminate()
                sentinel.wait(timeout=2)
            self.await_file(root / 'signals', process)
            process.send_signal(signal.SIGINT)
            process.send_signal(signal.SIGTERM)
            time.sleep(0.1)
            self.assertIsNone(process.poll(), 'cancellation must not claim quiescence')
            self.assertEqual((root / 'signals').read_text(), f'{signal.SIGINT}\n')
            (root / 'release').touch()
            stdout, stderr = process.communicate(timeout=5)
            self.assertEqual(process.returncode, 130, stderr)
            self.assertIn('client final log', stdout)
            self.assertEqual(stderr.count('Cancelling owned Bazel job group'), 1)
            self.assertTrue((root / 'leader_joined').exists())


if __name__ == "__main__":
    unittest.main()
