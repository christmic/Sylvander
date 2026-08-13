import importlib.util
import pathlib
import unittest


MODULE_PATH = pathlib.Path(__file__).with_name("podman_compat.py")
SPEC = importlib.util.spec_from_file_location("podman_compat", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
PODMAN_COMPAT = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PODMAN_COMPAT)


class TranslateArgumentsTest(unittest.TestCase):
    def test_non_compose_command_is_unchanged(self) -> None:
        arguments, directory = PODMAN_COMPAT.translate_arguments(
            ["version", "--format", "json"]
        )
        self.assertEqual(arguments, ["version", "--format", "json"])
        self.assertIsNone(directory)

    def test_compose_project_directory_becomes_working_directory(self) -> None:
        arguments, directory = PODMAN_COMPAT.translate_arguments(
            [
                "compose",
                "--project-name",
                "trial",
                "--project-directory",
                "/tmp/task",
                "-f",
                "compose.yaml",
                "up",
            ]
        )
        self.assertEqual(
            arguments,
            [
                "compose",
                "--project-name",
                "trial",
                "-f",
                "compose.yaml",
                "up",
            ],
        )
        self.assertEqual(directory, "/tmp/task")

    def test_compose_equals_form_is_supported(self) -> None:
        arguments, directory = PODMAN_COMPAT.translate_arguments(
            ["compose", "--project-directory=/tmp/task", "down"]
        )
        self.assertEqual(arguments, ["compose", "down"])
        self.assertEqual(directory, "/tmp/task")

    def test_missing_project_directory_fails(self) -> None:
        with self.assertRaisesRegex(ValueError, "requires a path"):
            PODMAN_COMPAT.translate_arguments(["compose", "--project-directory"])

    def test_duplicate_project_directory_fails(self) -> None:
        with self.assertRaisesRegex(ValueError, "only once"):
            PODMAN_COMPAT.translate_arguments(
                [
                    "compose",
                    "--project-directory=/tmp/one",
                    "--project-directory",
                    "/tmp/two",
                    "up",
                ]
            )


if __name__ == "__main__":
    unittest.main()
