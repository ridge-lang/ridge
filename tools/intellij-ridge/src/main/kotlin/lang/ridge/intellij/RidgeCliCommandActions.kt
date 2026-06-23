package lang.ridge.intellij

import com.intellij.execution.ExecutionException
import com.intellij.execution.RunContentExecutor
import com.intellij.execution.configurations.GeneralCommandLine
import com.intellij.execution.process.OSProcessHandler
import com.intellij.openapi.actionSystem.AnActionEvent
import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.project.Project
import com.intellij.openapi.ui.Messages
import com.intellij.openapi.util.SystemInfo
import com.redhat.devtools.lsp4ij.commands.LSPCommand
import com.redhat.devtools.lsp4ij.commands.LSPCommandAction
import java.io.File

/**
 * Find the `ridge` CLI, following the same chain as [RidgeLspResolver] for the
 * server binary: the canonical `cargo install` directory, then each `PATH`
 * entry, then the bare name. Anyone who installed `ridge-lsp` has `ridge` beside
 * it.
 */
private fun resolveRidgeCli(): String {
    val binName = if (SystemInfo.isWindows) "ridge.exe" else "ridge"

    val cargoBin = File(System.getProperty("user.home"))
        .resolve(".cargo")
        .resolve("bin")
        .resolve(binName)
    if (cargoBin.isFile) {
        return cargoBin.absolutePath
    }

    val path = System.getenv("PATH").orEmpty()
    for (dir in path.split(File.pathSeparatorChar)) {
        if (dir.isBlank()) continue
        val candidate = File(dir, binName)
        if (candidate.isFile) {
            return candidate.absolutePath
        }
    }

    return binName
}

/** `--member <project>` when a project name is present, nothing otherwise. */
private fun memberArgs(project: String?): List<String> =
    if (!project.isNullOrBlank()) listOf("--member", project) else emptyList()

/**
 * Run `ridge <params>` from the project root, streaming output to a console in
 * the Run tool window. The CLI resolves the workspace from its working
 * directory, so it is launched at [Project.getBasePath].
 */
private fun runRidge(project: Project, title: String, params: List<String>) {
    val commandLine = GeneralCommandLine(resolveRidgeCli())
        .withParameters(params)
        .withWorkDirectory(project.basePath)
    ApplicationManager.getApplication().invokeLater {
        try {
            val handler = OSProcessHandler(commandLine)
            RunContentExecutor(project, handler)
                .withTitle(title)
                .withActivateToolWindow(true)
                .run()
        } catch (e: ExecutionException) {
            Messages.showErrorDialog(
                project,
                e.message ?: "Failed to launch the `ridge` CLI.",
                "Ridge",
            )
        }
    }
}

/**
 * Handles the `ridge.run` code lens emitted by `ridge-lsp`: runs
 * `ridge run --member <project>`. The action `id` in plugin.xml matches the LSP
 * command id so LSP4IJ dispatches the lens to it client-side.
 */
class RidgeRunCommandAction : LSPCommandAction() {
    override fun commandPerformed(command: LSPCommand, event: AnActionEvent) {
        val project = event.project ?: return
        val member = command.getArgumentAt(0, String::class.java)
        runRidge(project, "Ridge Run", listOf("run") + memberArgs(member))
    }
}

/**
 * Handles the `ridge.test` code lens: runs
 * `ridge test --member <project> --filter *.<display-name>`. The CLI matches
 * `--filter` as a glob against `Module.<display-name>`, so the leading `*.`
 * anchors on the test's display name regardless of its module.
 */
class RidgeTestCommandAction : LSPCommandAction() {
    override fun commandPerformed(command: LSPCommand, event: AnActionEvent) {
        val project = event.project ?: return
        val member = command.getArgumentAt(0, String::class.java)
        val test = command.getArgumentAt(1, String::class.java)
        val filter = if (!test.isNullOrBlank()) listOf("--filter", "*.$test") else emptyList()
        runRidge(project, "Ridge Test", listOf("test") + memberArgs(member) + filter)
    }
}
