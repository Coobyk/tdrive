package dev.tdrive.app

import android.Manifest
import android.content.Context
import android.os.Build
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.BackHandler
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.compose.setContent
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.RowScope
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.Apps
import androidx.compose.material.icons.filled.ArrowBack
import androidx.compose.material.icons.filled.ArrowDownward
import androidx.compose.material.icons.filled.ArrowUpward
import androidx.compose.material.icons.filled.CloudUpload
import androidx.compose.material.icons.filled.ChevronRight
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material.icons.filled.DeleteForever
import androidx.compose.material.icons.filled.Download
import androidx.compose.material.icons.filled.DriveFolderUpload
import androidx.compose.material.icons.filled.Edit
import androidx.compose.material.icons.filled.Folder
import androidx.compose.material.icons.filled.FolderOff
import androidx.compose.material.icons.filled.Info
import androidx.compose.material.icons.filled.Lock
import androidx.compose.material.icons.filled.MoreVert
import androidx.compose.material.icons.filled.Refresh
import androidx.compose.material.icons.filled.Search
import androidx.compose.material.icons.filled.Undo
import androidx.compose.material.icons.filled.Upload
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.FilledTonalIconButton
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TextField
import androidx.compose.material3.TextFieldDefaults
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.text.style.TextOverflow
import androidx.documentfile.provider.DocumentFile
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import java.io.File
import java.io.FileOutputStream

class MainActivity : ComponentActivity() {

    private val core get() = Core.get()

    private val requestNotifications =
        registerForActivityResult(ActivityResultContracts.RequestPermission()) { /* best effort */ }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        core.start(applicationContext, filesDir.absolutePath)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            requestNotifications.launch(Manifest.permission.POST_NOTIFICATIONS)
        }
        setContent {
            TDriveTheme {
                Surface(Modifier.fillMaxSize(), color = MaterialTheme.colorScheme.surfaceContainerLowest) {
                    App(core, applicationContext)
                }
            }
        }
    }
}

@Composable
fun App(core: Core, ctx: Context) {
    val screen by core.screen.collectAsState()
    val status by core.status.collectAsState()

    // Cached API creds mean we've connected before — reconnect right
    // away so a returning user lands on sign-in/drives, not Setup.
    LaunchedEffect(Unit) {
        val prefs = ctx.getSharedPreferences("tdrive", Context.MODE_PRIVATE)
        val id = prefs.getString("api_id", "") ?: ""
        val hash = prefs.getString("api_hash", "") ?: ""
        if (id.isNotBlank() && hash.isNotBlank()) {
            core.connect(id.toIntOrNull() ?: 0, hash)
        }
    }

    Box(Modifier.fillMaxSize().statusBarsPadding().navigationBarsPadding()) {
        when (val s = screen) {
            Screen.Setup -> SetupScreen(core, ctx)
            Screen.Phone -> PhoneScreen(core)
            Screen.Code -> CodeScreen(core)
            is Screen.Password -> PasswordScreen(core, s.hint)
            Screen.Channels -> ChannelsScreen(core)
            Screen.Browser -> BrowserScreen(core, ctx)
        }
        if (status.isNotEmpty()) {
            Surface(
                Modifier
                    .align(Alignment.BottomCenter)
                    .padding(horizontal = 20.dp, vertical = 88.dp)
                    .clip(RoundedCornerShape(16.dp)),
                color = MaterialTheme.colorScheme.inverseSurface,
                shadowElevation = 8.dp,
            ) {
                Text(
                    status,
                    modifier = Modifier.padding(horizontal = 16.dp, vertical = 10.dp),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.inverseOnSurface,
                )
            }
        }
    }
}

@Composable
private fun Field(
    value: String,
    onChange: (String) -> Unit,
    label: String,
    password: Boolean = false,
    keyboard: KeyboardType = KeyboardType.Text,
) {
    TextField(
        value = value,
        onValueChange = onChange,
        label = { Text(label) },
        singleLine = true,
        shape = RoundedCornerShape(16.dp),
        visualTransformation = if (password) PasswordVisualTransformation()
        else androidx.compose.ui.text.input.VisualTransformation.None,
        keyboardOptions = if (password)
            KeyboardOptions(keyboardType = KeyboardType.Password, autoCorrectEnabled = false)
        else
            KeyboardOptions(keyboardType = keyboard),
        colors = TextFieldDefaults.colors(
            focusedIndicatorColor = Color.Transparent,
            unfocusedIndicatorColor = Color.Transparent,
            disabledIndicatorColor = Color.Transparent,
            errorIndicatorColor = Color.Transparent,
        ),
        modifier = Modifier.fillMaxWidth(),
    )
}

@Composable
private fun PrimaryButton(text: String, enabled: Boolean, onClick: () -> Unit) {
    FilledTonalButton(
        onClick = onClick,
        enabled = enabled,
        modifier = Modifier.fillMaxWidth().height(52.dp),
        shape = RoundedCornerShape(18.dp),
    ) {
        Text(text, style = MaterialTheme.typography.titleMedium)
    }
}

// ---------------------------------------------------------------------------
// Auth screens
// ---------------------------------------------------------------------------

@Composable
fun SetupScreen(core: Core, ctx: Context) {
    val prefs = remember { ctx.getSharedPreferences("tdrive", Context.MODE_PRIVATE) }
    var apiId by remember { mutableStateOf(prefs.getString("api_id", "") ?: "") }
    var apiHash by remember { mutableStateOf(prefs.getString("api_hash", "") ?: "") }

    AuthScaffold(
        title = "Connect your account",
        subtitle = "Grab your API ID and hash from my.telegram.org, then sign in with your phone number.",
    ) {
        val connecting by core.connecting.collectAsState()
        if (connecting) {
            Row(
                Modifier.fillMaxWidth().padding(vertical = 12.dp),
                horizontalArrangement = Arrangement.Center,
                verticalAlignment = Alignment.CenterVertically,
            ) {
                CircularProgressIndicator(Modifier.size(20.dp), strokeWidth = 2.dp)
                Spacer(Modifier.width(10.dp))
                Text("Connecting…", style = MaterialTheme.typography.bodyMedium)
            }
        } else {
            Field(apiId, { apiId = it }, "API ID", keyboard = KeyboardType.Number)
            Field(apiHash, { apiHash = it }, "API Hash")
        }
        PrimaryButton("Continue", apiId.isNotBlank() && apiHash.isNotBlank() && !connecting) {
            prefs.edit().putString("api_id", apiId).putString("api_hash", apiHash).apply()
            core.connect(apiId.toIntOrNull() ?: 0, apiHash)
        }
    }
}

@Composable
fun PhoneScreen(core: Core) {
    var phone by remember { mutableStateOf("") }
    AuthScaffold(title = "Welcome back", subtitle = "Enter the phone number on your Telegram account.") {
        Field(phone, { phone = it }, "Phone number (+1…)", keyboard = KeyboardType.Phone)
        PrimaryButton("Send code", phone.isNotBlank()) { core.requestCode(phone) }
    }
}

@Composable
fun CodeScreen(core: Core) {
    var code by remember { mutableStateOf("") }
    AuthScaffold(title = "Check your messages", subtitle = "We sent a login code to your Telegram app.") {
        Field(code, { code = it }, "Login code", keyboard = KeyboardType.Number)
        PrimaryButton("Sign in", code.isNotBlank()) { core.submitCode(code) }
    }
}

@Composable
fun PasswordScreen(core: Core, hint: String) {
    var pw by remember { mutableStateOf("") }
    AuthScaffold(title = "Two-factor password", subtitle = hint.ifEmpty { "Enter your Telegram cloud password." }) {
        Field(pw, { pw = it }, "Password", password = true)
        PrimaryButton("Unlock", pw.isNotBlank()) { core.submitPassword(pw) }
    }
}

// ---------------------------------------------------------------------------
// Drive list
// ---------------------------------------------------------------------------

@Composable
fun ChannelsScreen(core: Core) {
    val channels by core.channels.collectAsState()
    var newTitle by remember { mutableStateOf("") }
    var newSharedTitle by remember { mutableStateOf("") }

    Box(Modifier.fillMaxSize().background(MaterialTheme.colorScheme.surfaceContainerLowest)) {
        Column(Modifier.fillMaxSize().verticalScroll(rememberScrollState())) {
            BrandHeader(
                overline = "YOUR SPACE",
                title = "Your drives",
                subtitle = "${channels.size} drive${if (channels.size == 1) "" else "s"} ready to explore",
                actions = {
                    HeaderAction(Icons.Default.MoreVert, "Sign out") { core.signOut() }
                },
            )
            Column(Modifier.padding(horizontal = 16.dp, vertical = 20.dp), verticalArrangement = Arrangement.spacedBy(20.dp)) {
                Card(
                    shape = RoundedCornerShape(28.dp),
                    colors = CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.surfaceContainerLow),
                    elevation = CardDefaults.cardElevation(defaultElevation = 0.dp),
                ) {
                    Column(Modifier.fillMaxWidth().padding(20.dp), verticalArrangement = Arrangement.spacedBy(14.dp)) {
                        Text("Create a drive", style = MaterialTheme.typography.titleLarge)
                        Row(horizontalArrangement = Arrangement.spacedBy(10.dp), verticalAlignment = Alignment.CenterVertically) {
                            Box(
                                Modifier.size(46.dp).clip(CircleShape).background(MaterialTheme.colorScheme.primaryContainer),
                                contentAlignment = Alignment.Center,
                            ) { Icon(Icons.Default.Add, null, tint = MaterialTheme.colorScheme.onPrimaryContainer) }
                            TextField(
                                newTitle, { newTitle = it },
                                label = { Text("Personal drive") },
                                singleLine = true,
                                shape = RoundedCornerShape(16.dp),
                                colors = TextFieldDefaults.colors(
                                    focusedIndicatorColor = Color.Transparent,
                                    unfocusedIndicatorColor = Color.Transparent,
                                ),
                                modifier = Modifier.weight(1f),
                            )
                            FilledTonalIconButton(onClick = { core.createChannel(newTitle); newTitle = "" }, enabled = newTitle.isNotBlank()) {
                                Icon(Icons.Default.Add, "Create personal drive")
                            }
                        }
                        Row(horizontalArrangement = Arrangement.spacedBy(10.dp), verticalAlignment = Alignment.CenterVertically) {
                            Box(
                                Modifier.size(46.dp).clip(CircleShape).background(MaterialTheme.colorScheme.tertiaryContainer),
                                contentAlignment = Alignment.Center,
                            ) { Icon(Icons.Default.Apps, null, tint = MaterialTheme.colorScheme.onTertiaryContainer) }
                            TextField(
                                newSharedTitle, { newSharedTitle = it },
                                label = { Text("Shared drive") },
                                singleLine = true,
                                shape = RoundedCornerShape(16.dp),
                                colors = TextFieldDefaults.colors(
                                    focusedIndicatorColor = Color.Transparent,
                                    unfocusedIndicatorColor = Color.Transparent,
                                ),
                                modifier = Modifier.weight(1f),
                            )
                            FilledTonalIconButton(onClick = { core.createSharedChannel(newSharedTitle); newSharedTitle = "" }, enabled = newSharedTitle.isNotBlank()) {
                                Icon(Icons.Default.Add, "Create shared drive")
                            }
                        }
                    }
                }

                Text("Open a drive", style = MaterialTheme.typography.titleLarge, modifier = Modifier.padding(top = 4.dp))
                if (channels.isEmpty()) {
                    EmptyState(Icons.Default.Folder, "No drives yet", "Create a personal or shared drive above to get started.")
                } else {
                    Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
                        channels.forEach { c -> DriveCard(c.title, c.kind, onClick = { core.openChannel(c) }) }
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Browser
// ---------------------------------------------------------------------------

@OptIn(ExperimentalFoundationApi::class)
@Composable
fun BrowserScreen(core: Core, ctx: Context) {
    val entries by core.entries.collectAsState()
    val cwd by core.cwd.collectAsState()
    val title by core.driveTitle.collectAsState()
    val transfers by core.transfers.collectAsState()
    val trashMode by core.trashMode.collectAsState()
    val vault by core.vault.collectAsState()

    var selected by remember { mutableStateOf<NsEntry?>(null) }
    var showMkdir by remember { mutableStateOf(false) }
    var showRename by remember { mutableStateOf<NsEntry?>(null) }
    var showMove by remember { mutableStateOf<NsEntry?>(null) }
    var showUnlock by remember { mutableStateOf(false) }
    var showEnable by remember { mutableStateOf(false) }
    var showOptimize by remember { mutableStateOf(false) }
    var downloadFor by remember { mutableStateOf<NsEntry?>(null) }
    var showInfo by remember { mutableStateOf<NsEntry?>(null) }
    var searchActive by remember { mutableStateOf(false) }
    var query by remember { mutableStateOf("") }
    var sortSize by remember { mutableStateOf(false) }
    var sortDesc by remember { mutableStateOf(false) }
    var toolsMenu by remember { mutableStateOf(false) }

    val crumbs = remember(entries, cwd) {
        val path = mutableListOf<Pair<String, String>>()
        var cur: String? = cwd
        while (!cur.isNullOrEmpty()) {
            val e = entries.find { it.id == cur } ?: break
            path.add(0, e.id to e.name)
            cur = e.parent
        }
        path
    }

    val visible = remember(entries, cwd, trashMode, query, searchActive, sortSize, sortDesc) {
        val q = query.trim().lowercase()
        if (searchActive && q.isNotEmpty()) {
            entries.filter { !it.trashed && it.name.lowercase().contains(q) }
                .sortedBy { it.name.lowercase() }
        } else {
            val scoped = if (trashMode) entries.filter { it.trashed }
            else entries.filter { it.parent == cwd && !it.trashed }
            val byKey: Comparator<NsEntry> = if (sortSize)
                Comparator { a, b -> if (sortDesc) b.size.compareTo(a.size) else a.size.compareTo(b.size) }
            else
                Comparator { a, b -> if (sortDesc) b.name.lowercase().compareTo(a.name.lowercase()) else a.name.lowercase().compareTo(b.name.lowercase()) }
            scoped.sortedWith(compareBy<NsEntry> { it.kind != "dir" }.then(byKey))
        }
    }

    val activeTransfers = transfers.values.filter { it.state == "Active" || it.state == "Paused" }

    BackHandler(enabled = selected != null || searchActive || trashMode || cwd.isNotEmpty()) {
        when {
            selected != null -> selected = null
            searchActive -> { searchActive = false; query = "" }
            trashMode -> core.setTrashMode(false)
            else -> core.setCwd(entries.find { it.id == cwd }?.parent ?: "")
        }
    }
    BackHandler(enabled = selected == null && !searchActive && !trashMode && cwd.isEmpty()) {
        core.listChannels()
    }

    val headerTitle = when {
        trashMode -> "Trash"
        searchActive -> "Search"
        cwd.isEmpty() -> title.ifEmpty { "Your files" }
        else -> entries.find { it.id == cwd }?.name ?: title
    }
    val headerSubtitle = when {
        trashMode -> "Deleted items — restore or remove forever"
        searchActive -> "Find anything across all your drives"
        cwd.isEmpty() -> "Everything in this drive, all in one place"
        else -> "${visible.size} item${if (visible.size == 1) "" else "s"} here"
    }

    Column(Modifier.fillMaxSize().background(MaterialTheme.colorScheme.surfaceContainerLowest)) {
        // Slim M3 top bar — one 64dp row: up affordance when nested,
        // title+context inline, actions right. Breadcrumbs collapse to
        // a thin text row only when deep in the tree.
        val nested = !searchActive && !trashMode && cwd.isNotEmpty()
        Surface(color = MaterialTheme.colorScheme.surfaceContainerLow) {
            Column {
                Row(
                    Modifier.fillMaxWidth().height(64.dp).padding(horizontal = 8.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    if (nested) {
                        IconButton(onClick = {
                            core.setCwd(entries.find { it.id == cwd }?.parent ?: "")
                        }) { Icon(Icons.Default.ArrowBack, "Up one folder") }
                    } else {
                        BrandMark(size = 36.dp)
                    }
                    Column(Modifier.weight(1f).padding(horizontal = 10.dp)) {
                        Text(
                            headerTitle,
                            style = MaterialTheme.typography.titleMedium,
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                        )
                        Text(
                            headerSubtitle,
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                        )
                    }
                    IconButton(onClick = {
                        searchActive = !searchActive
                        if (!searchActive) query = ""
                    }) {
                        Icon(
                            Icons.Default.Search, "Search",
                            tint = if (searchActive) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                    IconButton(onClick = { core.setTrashMode(!trashMode); selected = null }) {
                        Icon(if (trashMode) Icons.Default.Folder else Icons.Default.Delete, "Trash")
                    }
                    IconButton(onClick = { core.listChannels() }) {
                        Icon(Icons.Default.Apps, "Drives")
                    }
                }
                if (nested) {
                    Row(
                        Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()).padding(horizontal = 16.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        crumbs.forEachIndexed { i, (id, name) ->
                            val last = i == crumbs.lastIndex
                            if (i > 0) {
                                Icon(
                                    Icons.Default.ChevronRight, null,
                                    tint = MaterialTheme.colorScheme.onSurfaceVariant,
                                    modifier = Modifier.size(14.dp),
                                )
                            }
                            Text(
                                name,
                                style = MaterialTheme.typography.labelMedium,
                                color = if (last) MaterialTheme.colorScheme.onSurface else MaterialTheme.colorScheme.onSurfaceVariant,
                                maxLines = 1,
                                modifier = Modifier
                                    .clickable(enabled = !last) { core.setCwd(id) }
                                    .padding(vertical = 8.dp, horizontal = 2.dp),
                            )
                        }
                    }
                }
            }
        }

        Column(Modifier.padding(horizontal = 16.dp, vertical = 12.dp), verticalArrangement = Arrangement.spacedBy(10.dp)) {
            if (searchActive) {
                TextField(
                    value = query,
                    onValueChange = { query = it },
                    placeholder = { Text("Search your whole drive…") },
                    leadingIcon = { Icon(Icons.Default.Search, null) },
                    trailingIcon = {
                        if (query.isNotEmpty()) IconButton(onClick = { query = "" }) { Icon(Icons.Default.Close, "Clear") }
                    },
                    singleLine = true,
                    shape = RoundedCornerShape(50),
                    colors = TextFieldDefaults.colors(
                        focusedIndicatorColor = Color.Transparent,
                        unfocusedIndicatorColor = Color.Transparent,
                        focusedContainerColor = MaterialTheme.colorScheme.surfaceContainerHigh,
                        unfocusedContainerColor = MaterialTheme.colorScheme.surfaceContainerHigh,
                    ),
                    modifier = Modifier.fillMaxWidth(),
                )
            }
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                TonalChip("Name", selected = !sortSize, onClick = { sortSize = false })
                TonalChip("Size", selected = sortSize, onClick = { sortSize = true })
                Spacer(Modifier.weight(1f))
                FilledTonalIconButton(onClick = { sortDesc = !sortDesc }) {
                    Icon(
                        if (sortDesc) Icons.Default.ArrowDownward else Icons.Default.ArrowUpward,
                        if (sortDesc) "Descending" else "Ascending",
                    )
                }
            }
        }

        LazyColumn(
            Modifier.weight(1f),
            contentPadding = androidx.compose.foundation.layout.PaddingValues(horizontal = 16.dp, vertical = 4.dp),
            verticalArrangement = Arrangement.spacedBy(10.dp),
        ) {
            items(visible, key = { it.id }) { e ->
                val bits = mutableListOf<String>()
                if (e.kind == "file") bits.add(humanSize(e.size))
                if (e.encrypted) bits.add("encrypted")
                if (searchActive && e.kind != "dir") {
                    bits.add("in ${entries.find { it.id == e.parent }?.name ?: "root"}")
                }
                DriveItemCard(
                    entry = e,
                    selected = selected?.id == e.id,
                    subtitle = if (bits.isEmpty()) null else bits.joinToString(" · "),
                    trailing = if (e.kind == "file") humanSize(e.size) else null,
                    onClick = {
                        if (e.kind == "dir" && !trashMode) core.setCwd(e.id)
                        else selected = if (selected?.id == e.id) null else e
                    },
                    onLongClick = { selected = e },
                    modifier = Modifier.animateItem(),
                )
            }
            if (visible.isEmpty()) {
                item {
                    EmptyState(
                        icon = if (searchActive) Icons.Default.Search else if (trashMode) Icons.Default.Delete else Icons.Default.FolderOff,
                        title = if (searchActive) "No matches" else if (trashMode) "Trash is empty" else "Nothing here yet",
                        subtitle = if (searchActive) "Try a different search term." else "Files you add will show up here.",
                    )
                }
            }
        }

        if (activeTransfers.isNotEmpty()) {
            Card(
                Modifier.padding(horizontal = 16.dp, vertical = 4.dp),
                shape = RoundedCornerShape(22.dp),
                colors = CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.tertiaryContainer),
                elevation = CardDefaults.cardElevation(defaultElevation = 0.dp),
            ) {
                Column(Modifier.fillMaxWidth().padding(14.dp), verticalArrangement = Arrangement.spacedBy(10.dp)) {
                    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                        Icon(Icons.Default.CloudUpload, null, tint = MaterialTheme.colorScheme.onTertiaryContainer, modifier = Modifier.size(18.dp))
                        Text("${activeTransfers.size} transfer${if (activeTransfers.size == 1) "" else "s"}", style = MaterialTheme.typography.titleSmall, color = MaterialTheme.colorScheme.onTertiaryContainer)
                    }
                    activeTransfers.take(3).forEach { t ->
                        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                            Text(t.label, Modifier.weight(1f), maxLines = 1, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onTertiaryContainer)
                            LinearProgressIndicator(
                                progress = { if (t.total > 0) (t.sent.toFloat() / t.total).coerceIn(0f, 1f) else 0f },
                                modifier = Modifier.width(90.dp).clip(RoundedCornerShape(50)),
                            )
                            IconButton(onClick = { core.cancelTransfer(t.id) }, modifier = Modifier.size(32.dp)) {
                                Icon(Icons.Default.Close, "Cancel", modifier = Modifier.size(16.dp), tint = MaterialTheme.colorScheme.onTertiaryContainer)
                            }
                        }
                    }
                }
            }
        }

        val sel = selected
        ActionDock {
            if (sel != null) {
                Text(sel.name, Modifier.weight(1f).padding(start = 8.dp), maxLines = 1, style = MaterialTheme.typography.titleSmall)
                if (trashMode) {
                    FilledTonalIconButton(onClick = { core.restore(sel.id); selected = null }) { Icon(Icons.Default.Undo, "Restore") }
                    FilledTonalIconButton(onClick = { core.purge(sel.id); selected = null }) { Icon(Icons.Default.DeleteForever, "Delete forever") }
                } else {
                    if (sel.kind == "file") FilledTonalIconButton(onClick = { downloadFor = sel }) { Icon(Icons.Default.Download, "Download") }
                    IconButton(onClick = { showInfo = sel }) { Icon(Icons.Default.Info, "Properties") }
                    IconButton(onClick = { showRename = sel }) { Icon(Icons.Default.Edit, "Rename") }
                    IconButton(onClick = { showMove = sel }) { Icon(Icons.Default.Folder, "Move") }
                    IconButton(onClick = { core.delete(sel.id); selected = null }) { Icon(Icons.Default.Delete, "Delete") }
                }
                IconButton(onClick = { selected = null }) { Icon(Icons.Default.Close, "Deselect") }
            } else {
                if (!trashMode) {
                    UploadButton(core, ctx)
                    FilledTonalButton(onClick = { showMkdir = true }, modifier = Modifier.height(44.dp)) {
                        Icon(Icons.Default.Add, null, modifier = Modifier.size(18.dp))
                        Spacer(Modifier.width(6.dp))
                        Text("New folder")
                    }
                    if (vault.unlocked) {
                        Icon(Icons.Default.Lock, "Encrypted", tint = MaterialTheme.colorScheme.primary, modifier = Modifier.size(16.dp))
                    }
                    Spacer(Modifier.weight(1f))
                    IconButton(onClick = { core.sync() }) { Icon(Icons.Default.Refresh, "Sync") }
                    IconButton(onClick = { toolsMenu = true }) { Icon(Icons.Default.MoreVert, "Tools") }
                    DropdownMenu(expanded = toolsMenu, onDismissRequest = { toolsMenu = false }) {
                        if (vault.configured) {
                            DropdownMenuItem(
                                text = { Text(if (vault.unlocked) "Lock vault" else "Unlock vault") },
                                onClick = { toolsMenu = false; if (vault.unlocked) core.lock() else showUnlock = true },
                            )
                        } else {
                            DropdownMenuItem(text = { Text("Enable encryption") }, onClick = { toolsMenu = false; showEnable = true })
                        }
                        DropdownMenuItem(text = { Text("Optimize drive") }, onClick = { toolsMenu = false; showOptimize = true })
                        DropdownMenuItem(text = { Text("Resume uploads") }, onClick = { toolsMenu = false; core.resumeUploads() })
                    }
                } else {
                    FilledTonalButton(onClick = { core.emptyTrash() }) { Text("Empty trash") }
                    Spacer(Modifier.weight(1f))
                    IconButton(onClick = { toolsMenu = true }) { Icon(Icons.Default.MoreVert, "Tools") }
                }
            }
        }
    }

    // Dialogs
    if (showMkdir) InputDialog("New folder", "Folder name") { name ->
        showMkdir = false
        if (name.isNotBlank()) core.mkdir(name)
    }
    showRename?.let { e ->
        InputDialog("Rename", "New name", e.name) { name ->
            showRename = null
            if (name.isNotBlank()) core.rename(e.id, name)
        }
    }
    showMove?.let { e ->
        val folders = entries.filter { it.kind == "dir" && !it.trashed && it.id != e.id }
        Dialog(onDismissRequest = { showMove = null }) {
            Surface(shape = RoundedCornerShape(28.dp), color = MaterialTheme.colorScheme.surfaceContainerHigh) {
                Column(Modifier.padding(20.dp)) {
                    Text("Move “${e.name}” to", style = MaterialTheme.typography.titleLarge)
                    Spacer(Modifier.height(12.dp))
                    LazyColumn(Modifier.heightIn(max = 300.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
                        item { ListItem(headlineContent = { Text("root") }, modifier = Modifier.clickable { core.move(e.id, ""); showMove = null }) }
                        items(folders, key = { it.id }) { f ->
                            ListItem(headlineContent = { Text(f.name) }, modifier = Modifier.clickable { core.move(e.id, f.id); showMove = null })
                        }
                    }
                }
            }
        }
    }
    downloadFor?.let { e ->
        AlertDialog(
            onDismissRequest = { downloadFor = null },
            shape = RoundedCornerShape(28.dp),
            icon = { Icon(Icons.Default.Download, null) },
            title = { Text("Download ${e.name}") },
            text = { Text("Save to the app files dir (${humanSize(e.size)}).") },
            confirmButton = { TextButton(onClick = { core.download(e.id, e.name); downloadFor = null }) { Text("Download") } },
            dismissButton = { TextButton(onClick = { downloadFor = null }) { Text("Cancel") } },
        )
    }
    showInfo?.let { e ->
        val rows = mutableListOf<Pair<String, String>>()
        rows.add("type" to (if (e.kind == "dir") "folder" else if (e.encrypted) "file (encrypted)" else "file"))
        rows.add("name" to e.name)
        if (e.kind == "file") {
            rows.add("size" to "${humanSize(e.size)} (${e.size} B)")
            rows.add("uploaded" to fmtTs(e.uploadTime))
            if (e.partCount > 1) rows.add("parts" to "${e.partCount} chunks")
        }
        rows.add("location" to (entries.find { it.id == e.parent }?.name ?: "root"))
        AlertDialog(
            onDismissRequest = { showInfo = null },
            shape = RoundedCornerShape(28.dp),
            icon = { Icon(Icons.Default.Info, null) },
            title = { Text("Properties") },
            text = {
                Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
                    rows.forEach { (k, v) ->
                        Row {
                            Text(k, Modifier.width(80.dp), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                            Text(v, style = MaterialTheme.typography.bodyMedium)
                        }
                    }
                }
            },
            confirmButton = { TextButton(onClick = { showInfo = null }) { Text("Close") } },
        )
    }
    if (showUnlock) InputDialog("Unlock vault", "Password", password = true) { pw ->
        showUnlock = false
        core.unlock(pw)
    }
    if (showEnable) {
        var pw by remember { mutableStateOf("") }
        var hint by remember { mutableStateOf("") }
        AlertDialog(
            onDismissRequest = { showEnable = false },
            shape = RoundedCornerShape(28.dp),
            title = { Text("Enable encryption") },
            text = {
                Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                    Field(pw, { pw = it }, "Password", password = true)
                    Field(hint, { hint = it }, "Hint (optional)")
                }
            },
            confirmButton = { TextButton(onClick = { showEnable = false; core.enableEncryption(pw, hint) }, enabled = pw.isNotBlank()) { Text("Enable") } },
            dismissButton = { TextButton(onClick = { showEnable = false }) { Text("Cancel") } },
        )
    }
    if (showOptimize) {
        AlertDialog(
            onDismissRequest = { showOptimize = false },
            shape = RoundedCornerShape(28.dp),
            title = { Text("Optimize drive") },
            text = { Text("Scan the channel and delete messages belonging to objects that were created and later removed.") },
            confirmButton = { TextButton(onClick = { showOptimize = false; core.optimize() }) { Text("Scan") } },
            dismissButton = { TextButton(onClick = { showOptimize = false }) { Text("Cancel") } },
        )
    }
}

@Composable
private fun RowScope.HeaderAction(icon: ImageVector, desc: String, active: Boolean = false, onClick: () -> Unit) {
    IconButton(
        onClick = onClick,
        modifier = Modifier
            .size(44.dp)
            .clip(CircleShape)
            .background(Color.White.copy(alpha = if (active) 0.32f else 0.16f)),
    ) {
        Icon(icon, desc, tint = Color.White)
    }
}

private fun uriName(ctx: Context, uri: android.net.Uri): String =
    ctx.contentResolver.query(uri, null, null, null, null)?.use { c ->
        val i = c.getColumnIndex(android.provider.OpenableColumns.DISPLAY_NAME)
        if (c.moveToFirst() && i >= 0) c.getString(i) else null
    } ?: uri.lastPathSegment?.substringAfterLast('/') ?: "upload"

private fun copyUri(ctx: Context, uri: android.net.Uri, dst: File): Boolean =
    try {
        ctx.contentResolver.openInputStream(uri)?.use { inp ->
            FileOutputStream(dst).use { out -> inp.copyTo(out) }
        }
        true
    } catch (_: Exception) {
        false
    }

/// Recursively mirror a SAF document tree into `dst` so the upload
/// sees real files in a real directory structure.
private fun copyTree(ctx: Context, node: DocumentFile, dst: File) {
    for (child in node.listFiles()) {
        val name = child.name ?: continue
        if (child.isDirectory) {
            val sub = File(dst, name)
            if (sub.mkdirs() || sub.isDirectory) copyTree(ctx, child, sub)
        } else {
            copyUri(ctx, child.uri, File(dst, name))
        }
    }
}

@Composable
fun UploadButton(core: Core, ctx: Context) {
    val scope = rememberCoroutineScope()
    val filesLauncher = rememberLauncherForActivityResult(
        ActivityResultContracts.OpenMultipleDocuments()
    ) { uris ->
        if (uris.isEmpty()) return@rememberLauncherForActivityResult
        scope.launch {
            withContext(Dispatchers.IO) {
                uris.forEach { uri ->
                    val dst = File(
                        ctx.cacheDir,
                        "inbox/${System.currentTimeMillis()}-${uriName(ctx, uri)}",
                    )
                    dst.parentFile?.mkdirs()
                    if (copyUri(ctx, uri, dst)) core.upload(dst.absolutePath)
                }
            }
        }
    }
    val dirLauncher = rememberLauncherForActivityResult(
        ActivityResultContracts.OpenDocumentTree()
    ) { uri ->
        uri ?: return@rememberLauncherForActivityResult
        scope.launch {
            withContext(Dispatchers.IO) {
                val root = DocumentFile.fromTreeUri(ctx, uri) ?: return@withContext
                val dst = File(
                    ctx.cacheDir,
                    "inbox/${System.currentTimeMillis()}-${root.name ?: "folder"}",
                )
                dst.mkdirs()
                copyTree(ctx, root, dst)
                core.uploadDir(dst.absolutePath)
            }
        }
    }
    Row(horizontalArrangement = Arrangement.spacedBy(4.dp)) {
        FilledTonalButton(onClick = { filesLauncher.launch(arrayOf("*/*")) }, modifier = Modifier.height(44.dp)) {
            Icon(Icons.Default.Upload, null, modifier = Modifier.size(18.dp))
            Spacer(Modifier.width(6.dp))
            Text("Files")
        }
        FilledTonalIconButton(onClick = { dirLauncher.launch(null) }, modifier = Modifier.size(44.dp)) {
            Icon(Icons.Default.DriveFolderUpload, "Upload folder", modifier = Modifier.size(20.dp))
        }
    }
}

@Composable
fun InputDialog(title: String, label: String, initial: String = "", password: Boolean = false, onDone: (String) -> Unit) {
    var text by remember { mutableStateOf(initial) }
    AlertDialog(
        onDismissRequest = { onDone("") },
        shape = RoundedCornerShape(28.dp),
        title = { Text(title) },
        text = { Field(text, { text = it }, label, password = password) },
        confirmButton = { TextButton(onClick = { onDone(text) }) { Text("OK") } },
        dismissButton = { TextButton(onClick = { onDone("") }) { Text("Cancel") } },
    )
}

fun fmtTs(t: Long): String {
    if (t <= 0) return "—"
    return java.text.SimpleDateFormat("yyyy-MM-dd HH:mm", java.util.Locale.US)
        .format(java.util.Date(t * 1000))
}

fun humanSize(n: Long): String {
    if (n < 1024) return "$n B"
    val units = arrayOf("KiB", "MiB", "GiB", "TiB")
    var v = n.toDouble()
    var i = -1
    do { v /= 1024; i++ } while (v >= 1024 && i < units.lastIndex)
    return "%.1f %s".format(v, units[i])
}
