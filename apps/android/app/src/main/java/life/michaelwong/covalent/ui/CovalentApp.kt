package life.michaelwong.covalent.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.rounded.AddLink
import androidx.compose.material.icons.rounded.Backup
import androidx.compose.material.icons.rounded.FolderOpen
import androidx.compose.material.icons.rounded.Security
import androidx.compose.material.icons.rounded.Storage
import androidx.compose.material3.Card
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.tooling.preview.Preview
import androidx.compose.ui.unit.dp
import life.michaelwong.covalent.model.PrimaryAction
import life.michaelwong.covalent.ui.theme.CovalentTheme

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun CovalentApp() {
    Scaffold(
        topBar = {
            TopAppBar(
                title = {
                    Column {
                        Text("Covalent", fontWeight = FontWeight.SemiBold)
                        Text(
                            "Private distributed backup",
                            style = MaterialTheme.typography.labelMedium,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                },
            )
        },
        floatingActionButton = {
            PrimaryActionToolbar(
                enabled = false,
                onAction = {},
            )
        },
        floatingActionButtonPosition = androidx.compose.material3.FabPosition.Center,
    ) { innerPadding ->
        Overview(
            contentPadding = PaddingValues(
                start = 20.dp,
                top = innerPadding.calculateTopPadding() + 12.dp,
                end = 20.dp,
                bottom = 112.dp,
            ),
        )
    }
}

@Composable
private fun Overview(contentPadding: PaddingValues) {
    LazyColumn(
        modifier = Modifier.fillMaxSize(),
        contentPadding = contentPadding,
        verticalArrangement = Arrangement.spacedBy(16.dp),
    ) {
        item {
            Column {
                Text(
                    "Your copies. Your devices.",
                    style = MaterialTheme.typography.headlineMedium,
                    fontWeight = FontWeight.SemiBold,
                )
                Spacer(Modifier.height(8.dp))
                Text(
                    "Pair directly, choose every replica, and restore only to a folder you authorize.",
                    style = MaterialTheme.typography.bodyLarge,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
        item {
            StatusCard(
                title = "Engine service",
                value = "Not connected",
                detail = "Start the local Covalent service to enable Pair, Backup, and Restore.",
                icon = { Icon(Icons.Rounded.Storage, contentDescription = null) },
            )
        }
        item {
            StatusCard(
                title = "Replica policy",
                value = "Explicit selection",
                detail = "Covalent never chooses a storage device for you.",
                icon = { Icon(Icons.Rounded.Security, contentDescription = null) },
            )
        }
    }
}

@Composable
private fun StatusCard(
    title: String,
    value: String,
    detail: String,
    icon: @Composable () -> Unit,
) {
    Card(modifier = Modifier.fillMaxWidth()) {
        Row(
            modifier = Modifier.padding(20.dp),
            horizontalArrangement = Arrangement.spacedBy(16.dp),
            verticalAlignment = Alignment.Top,
        ) {
            Surface(
                shape = MaterialTheme.shapes.medium,
                color = MaterialTheme.colorScheme.secondaryContainer,
            ) {
                Box(Modifier.padding(12.dp)) { icon() }
            }
            Column {
                Text(title, style = MaterialTheme.typography.labelLarge)
                Text(
                    value,
                    style = MaterialTheme.typography.titleLarge,
                    fontWeight = FontWeight.SemiBold,
                )
                Spacer(Modifier.height(6.dp))
                Text(
                    detail,
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
    }
}

@Composable
fun PrimaryActionToolbar(
    enabled: Boolean,
    onAction: (PrimaryAction) -> Unit,
) {
    Surface(
        modifier = Modifier
            .semantics { contentDescription = "Primary actions" }
            .padding(horizontal = 12.dp),
        shape = MaterialTheme.shapes.extraLarge,
        color = MaterialTheme.colorScheme.surfaceContainerHigh,
        tonalElevation = 6.dp,
        shadowElevation = 3.dp,
    ) {
        Row(
            modifier = Modifier.padding(8.dp),
            horizontalArrangement = Arrangement.spacedBy(8.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            ToolbarButton(PrimaryAction.PAIR, enabled, onAction) {
                Icon(Icons.Rounded.AddLink, contentDescription = null)
            }
            ToolbarButton(PrimaryAction.BACKUP, enabled, onAction) {
                Icon(Icons.Rounded.Backup, contentDescription = null)
            }
            ToolbarButton(PrimaryAction.RESTORE, enabled, onAction) {
                Icon(Icons.Rounded.FolderOpen, contentDescription = null)
            }
        }
    }
}

@Composable
private fun ToolbarButton(
    action: PrimaryAction,
    enabled: Boolean,
    onAction: (PrimaryAction) -> Unit,
    icon: @Composable () -> Unit,
) {
    FilledTonalButton(
        onClick = { onAction(action) },
        enabled = enabled,
        contentPadding = PaddingValues(horizontal = 14.dp, vertical = 12.dp),
    ) {
        icon()
        Text(action.label, modifier = Modifier.padding(start = 7.dp))
    }
}

@Preview(showBackground = true, widthDp = 420, heightDp = 820)
@Composable
private fun AppPreview() {
    CovalentTheme { CovalentApp() }
}
