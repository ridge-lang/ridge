package lang.ridge.intellij

import com.intellij.ide.FileIconProvider
import com.intellij.openapi.project.Project
import com.intellij.openapi.vfs.VirtualFile
import javax.swing.Icon

/**
 * Shows the Ridge icon for `*.ridge` files. The file type itself is owned by
 * the bundled TextMate grammar (so the IDE highlights the syntax), and an icon
 * provider is how a custom icon rides along with a TextMate-managed extension.
 */
class RidgeFileIconProvider : FileIconProvider {
    override fun getIcon(file: VirtualFile, flags: Int, project: Project?): Icon? =
        if (file.extension.equals("ridge", ignoreCase = true)) RidgeIcons.FILE else null
}
