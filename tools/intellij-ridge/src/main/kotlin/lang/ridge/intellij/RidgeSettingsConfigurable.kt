package lang.ridge.intellij

import com.intellij.openapi.fileChooser.FileChooserDescriptorFactory
import com.intellij.openapi.options.Configurable
import com.intellij.openapi.ui.TextBrowseFolderListener
import com.intellij.openapi.ui.TextFieldWithBrowseButton
import com.intellij.ui.components.JBLabel
import com.intellij.util.ui.FormBuilder
import javax.swing.JComponent
import javax.swing.JPanel

/**
 * A settings page under **Languages & Frameworks | Ridge** that overrides the
 * `ridge-lsp` location. Empty means auto-resolve (see [RidgeLspResolver]); a
 * path here wins over `~/.cargo/bin` and `PATH`.
 */
class RidgeSettingsConfigurable : Configurable {
    private var pathField: TextFieldWithBrowseButton? = null

    override fun getDisplayName(): String = "Ridge"

    override fun createComponent(): JComponent {
        val field = TextFieldWithBrowseButton()
        field.addBrowseFolderListener(
            TextBrowseFolderListener(FileChooserDescriptorFactory.createSingleFileDescriptor()),
        )
        field.text = RidgeSettings.getInstance().lspPath
        pathField = field

        return FormBuilder.createFormBuilder()
            .addLabeledComponent(JBLabel("Path to ridge-lsp:"), field, 1, false)
            .addComponentToRightColumn(
                JBLabel("Leave empty to auto-resolve from ~/.cargo/bin or PATH."),
            )
            .addComponentFillVertically(JPanel(), 0)
            .panel
    }

    override fun isModified(): Boolean =
        (pathField?.text?.trim() ?: "") != RidgeSettings.getInstance().lspPath

    override fun apply() {
        RidgeSettings.getInstance().lspPath = pathField?.text?.trim() ?: ""
    }

    override fun reset() {
        pathField?.text = RidgeSettings.getInstance().lspPath
    }

    override fun getPreferredFocusedComponent(): JComponent? = pathField

    override fun disposeUIResources() {
        pathField = null
    }
}
