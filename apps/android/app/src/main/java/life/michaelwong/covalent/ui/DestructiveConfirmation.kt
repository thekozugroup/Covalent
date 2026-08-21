package life.michaelwong.covalent.ui

import androidx.compose.material3.AlertDialog
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import life.michaelwong.covalent.R

/**
 * Every Android action that destroys work, trust, or remembered state.
 *
 * Each entry must map to a confirmation whose message names the exact thing being
 * destroyed and says what cannot be undone. `destructiveConfirmation` is pure so the
 * copy contract stays unit-testable without an instrumented device.
 */
internal enum class DestructiveAction {
    CANCEL_TRANSFER,
    DISABLE_PHONE_STORAGE,
    DISCARD_PAIRING_PROGRESS,
    CANCEL_DEVICE_REQUEST,
    IMPORT_REMOVING_BACKUPS,
}

/**
 * Copy for one destructive confirmation. [namesSubject] is true when [messageRes]
 * carries a single `%1$s` placeholder that must be filled with the name of the thing
 * being destroyed.
 */
internal data class DestructiveConfirmation(
    val titleRes: Int,
    val messageRes: Int,
    val confirmRes: Int,
    val cancelRes: Int,
    val namesSubject: Boolean,
)

internal fun destructiveConfirmation(action: DestructiveAction): DestructiveConfirmation =
    when (action) {
        DestructiveAction.CANCEL_TRANSFER -> DestructiveConfirmation(
            titleRes = R.string.confirm_cancel_transfer_title,
            messageRes = R.string.confirm_cancel_transfer_message,
            confirmRes = R.string.confirm_cancel_transfer_action,
            cancelRes = R.string.confirm_cancel_transfer_keep,
            namesSubject = true,
        )
        DestructiveAction.DISABLE_PHONE_STORAGE -> DestructiveConfirmation(
            titleRes = R.string.confirm_disable_phone_storage_title,
            messageRes = R.string.confirm_disable_phone_storage_message,
            confirmRes = R.string.confirm_disable_phone_storage_action,
            cancelRes = R.string.confirm_disable_phone_storage_keep,
            namesSubject = false,
        )
        DestructiveAction.DISCARD_PAIRING_PROGRESS -> DestructiveConfirmation(
            titleRes = R.string.confirm_discard_pairing_title,
            messageRes = R.string.confirm_discard_pairing_message,
            confirmRes = R.string.confirm_discard_pairing_action,
            cancelRes = R.string.confirm_discard_pairing_keep,
            namesSubject = false,
        )
        DestructiveAction.CANCEL_DEVICE_REQUEST -> DestructiveConfirmation(
            titleRes = R.string.confirm_cancel_device_request_title,
            messageRes = R.string.confirm_cancel_device_request_message,
            confirmRes = R.string.confirm_cancel_device_request_action,
            cancelRes = R.string.confirm_cancel_device_request_keep,
            namesSubject = true,
        )
        DestructiveAction.IMPORT_REMOVING_BACKUPS -> DestructiveConfirmation(
            titleRes = R.string.confirm_import_removes_backups_title,
            messageRes = R.string.confirm_import_removes_backups_message,
            confirmRes = R.string.confirm_import_removes_backups_action,
            cancelRes = R.string.confirm_import_removes_backups_keep,
            namesSubject = true,
        )
    }

/**
 * Material 3 confirmation for a destructive action. The confirming button is tinted
 * with the error color and the dismissing button always keeps the current state.
 */
@Composable
internal fun DestructiveConfirmDialog(
    action: DestructiveAction,
    subject: String = "",
    onConfirm: () -> Unit,
    onDismiss: () -> Unit,
) {
    val copy = destructiveConfirmation(action)
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(stringResource(copy.titleRes)) },
        text = {
            Text(
                if (copy.namesSubject) {
                    stringResource(copy.messageRes, subject)
                } else {
                    stringResource(copy.messageRes)
                },
            )
        },
        confirmButton = {
            TextButton(
                onClick = onConfirm,
                modifier = Modifier.testTag("confirm.${action.name}.proceed"),
            ) {
                Text(stringResource(copy.confirmRes), color = MaterialTheme.colorScheme.error)
            }
        },
        dismissButton = {
            TextButton(
                onClick = onDismiss,
                modifier = Modifier.testTag("confirm.${action.name}.cancel"),
            ) {
                Text(stringResource(copy.cancelRes))
            }
        },
        modifier = Modifier.testTag("confirm.${action.name}"),
    )
}
