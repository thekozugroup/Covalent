package life.michaelwong.covalent.sync;

import android.database.Cursor;
import android.database.MatrixCursor;
import android.net.Uri;
import android.os.Bundle;
import android.os.CancellationSignal;
import android.os.ParcelFileDescriptor;
import android.provider.DocumentsContract.Document;
import android.provider.DocumentsContract.Root;
import android.provider.DocumentsProvider;

import java.io.File;
import java.io.FileNotFoundException;
import java.io.IOException;
import java.nio.file.Files;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.UUID;

/** Mutable test-APK-only SAF provider used to exercise the loopback WebDAV adapter. */
public final class SafWebDavTestDocumentsProvider extends DocumentsProvider {
    public static final String AUTHORITY = "life.michaelwong.covalent.test.saf-webdav";
    public static final String ROOT_ID = "root:opaque";
    public static final String METHOD_RESET = "reset";

    private static final String[] DOCUMENT_PROJECTION = new String[] {
        Document.COLUMN_DOCUMENT_ID,
        Document.COLUMN_DISPLAY_NAME,
        Document.COLUMN_MIME_TYPE,
        Document.COLUMN_FLAGS,
        Document.COLUMN_SIZE,
        Document.COLUMN_LAST_MODIFIED,
    };
    private static final String[] ROOT_PROJECTION = new String[] {
        Root.COLUMN_ROOT_ID,
        Root.COLUMN_DOCUMENT_ID,
        Root.COLUMN_TITLE,
        Root.COLUMN_FLAGS,
    };

    private final Map<String, Node> nodes = new LinkedHashMap<>();
    private File storage;

    @Override
    public synchronized boolean onCreate() {
        storage = new File(getContext().getCacheDir(), "saf-webdav-provider");
        reset();
        return true;
    }

    @Override
    public synchronized Bundle call(String method, String argument, Bundle extras) {
        Bundle result = super.call(method, argument, extras);
        if (result != null || !METHOD_RESET.equals(method)) return result;
        reset();
        return Bundle.EMPTY;
    }

    @Override
    public synchronized Cursor queryRoots(String[] projection) {
        String[] columns = projection == null ? ROOT_PROJECTION : projection;
        MatrixCursor cursor = new MatrixCursor(columns);
        Object[] row = new Object[columns.length];
        for (int index = 0; index < columns.length; index += 1) {
            if (Root.COLUMN_ROOT_ID.equals(columns[index])) row[index] = ROOT_ID;
            else if (Root.COLUMN_DOCUMENT_ID.equals(columns[index])) row[index] = ROOT_ID;
            else if (Root.COLUMN_TITLE.equals(columns[index])) row[index] = "Covalent WebDAV fixture";
            else if (Root.COLUMN_FLAGS.equals(columns[index])) {
                row[index] = Root.FLAG_SUPPORTS_CREATE | Root.FLAG_SUPPORTS_IS_CHILD;
            }
        }
        cursor.addRow(row);
        return cursor;
    }

    @Override
    public synchronized Cursor queryDocument(String documentId, String[] projection)
            throws FileNotFoundException {
        return documents(projection, List.of(requireNode(documentId)));
    }

    @Override
    public synchronized Cursor queryChildDocuments(
            String parentDocumentId,
            String[] projection,
            String sortOrder
    ) throws FileNotFoundException {
        requireDirectory(parentDocumentId);
        List<Node> children = new ArrayList<>();
        for (Node node : nodes.values()) {
            if (parentDocumentId.equals(node.parentId)) children.add(node);
        }
        return documents(projection, children);
    }

    @Override
    public synchronized String createDocument(String parentDocumentId, String mimeType, String displayName)
            throws FileNotFoundException {
        Node parent = requireDirectory(parentDocumentId);
        requireAvailableName(parentDocumentId, displayName, null);
        boolean directory = Document.MIME_TYPE_DIR.equals(mimeType);
        String id = "doc:" + UUID.randomUUID();
        File file = new File(storage, id.substring(4));
        try {
            if (directory ? !file.mkdir() : !file.createNewFile()) throw new IOException("create failed");
        } catch (IOException error) {
            throw new FileNotFoundException("Could not create test document");
        }
        nodes.put(id, new Node(id, parent.id, displayName, mimeType, file));
        return id;
    }

    @Override
    public synchronized void deleteDocument(String documentId) throws FileNotFoundException {
        if (ROOT_ID.equals(documentId)) throw new FileNotFoundException("Cannot remove root");
        requireNode(documentId);
        deleteRecursively(documentId);
    }

    @Override
    public synchronized String renameDocument(String documentId, String displayName)
            throws FileNotFoundException {
        Node node = requireNode(documentId);
        if (ROOT_ID.equals(documentId)) throw new FileNotFoundException("Cannot rename root");
        requireAvailableName(node.parentId, displayName, documentId);
        node.name = displayName;
        return documentId;
    }

    @Override
    public synchronized String moveDocument(
            String sourceDocumentId,
            String sourceParentDocumentId,
            String targetParentDocumentId
    ) throws FileNotFoundException {
        Node source = requireNode(sourceDocumentId);
        if (!sourceParentDocumentId.equals(source.parentId)) throw new FileNotFoundException("Wrong source parent");
        requireDirectory(targetParentDocumentId);
        if (source.directory() && isChildDocument(sourceDocumentId, targetParentDocumentId)) {
            throw new FileNotFoundException("Cycle");
        }
        requireAvailableName(targetParentDocumentId, source.name, source.id);
        source.parentId = targetParentDocumentId;
        return source.id;
    }

    @Override
    public synchronized boolean isChildDocument(String parentDocumentId, String documentId) {
        Node current = nodes.get(documentId);
        while (current != null && current.parentId != null) {
            if (parentDocumentId.equals(current.parentId)) return true;
            current = nodes.get(current.parentId);
        }
        return false;
    }

    @Override
    public synchronized ParcelFileDescriptor openDocument(
            String documentId,
            String mode,
            CancellationSignal signal
    ) throws FileNotFoundException {
        Node node = requireNode(documentId);
        if (node.directory()) throw new FileNotFoundException("Directory has no content");
        return ParcelFileDescriptor.open(node.file, ParcelFileDescriptor.parseMode(mode));
    }

    private Cursor documents(String[] projection, List<Node> selected) {
        String[] columns = projection == null ? DOCUMENT_PROJECTION : projection;
        MatrixCursor cursor = new MatrixCursor(columns);
        for (Node node : selected) {
            Object[] row = new Object[columns.length];
            for (int index = 0; index < columns.length; index += 1) {
                String column = columns[index];
                if (Document.COLUMN_DOCUMENT_ID.equals(column)) row[index] = node.id;
                else if (Document.COLUMN_DISPLAY_NAME.equals(column)) row[index] = node.name;
                else if (Document.COLUMN_MIME_TYPE.equals(column)) row[index] = node.mimeType;
                else if (Document.COLUMN_FLAGS.equals(column)) {
                    row[index] = Document.FLAG_SUPPORTS_DELETE | Document.FLAG_SUPPORTS_RENAME |
                        Document.FLAG_SUPPORTS_MOVE | (node.directory() ? Document.FLAG_DIR_SUPPORTS_CREATE :
                        Document.FLAG_SUPPORTS_WRITE);
                } else if (Document.COLUMN_SIZE.equals(column)) {
                    row[index] = node.directory() ? null : node.file.length();
                } else if (Document.COLUMN_LAST_MODIFIED.equals(column)) row[index] = node.file.lastModified();
            }
            cursor.addRow(row);
        }
        return cursor;
    }

    private Node requireNode(String id) throws FileNotFoundException {
        Node node = nodes.get(id);
        if (node == null) throw new FileNotFoundException("Unknown test document");
        return node;
    }

    private Node requireDirectory(String id) throws FileNotFoundException {
        Node node = requireNode(id);
        if (!node.directory()) throw new FileNotFoundException("Not a directory");
        return node;
    }

    private void requireAvailableName(String parentId, String name, String exceptId) throws FileNotFoundException {
        if (name == null || name.isEmpty()) throw new FileNotFoundException("Invalid name");
        for (Node node : nodes.values()) {
            if (parentId.equals(node.parentId) && name.equals(node.name) && !node.id.equals(exceptId)) {
                throw new FileNotFoundException("Duplicate name");
            }
        }
    }

    private void deleteRecursively(String id) {
        List<String> children = new ArrayList<>();
        for (Node node : nodes.values()) if (id.equals(node.parentId)) children.add(node.id);
        for (String child : children) deleteRecursively(child);
        Node removed = nodes.remove(id);
        if (removed != null && !removed.file.delete()) removed.file.deleteOnExit();
    }

    private void reset() {
        if (storage.exists()) deleteFileTree(storage);
        if (!storage.mkdirs() && !storage.isDirectory()) throw new IllegalStateException("Could not reset fixture");
        nodes.clear();
        nodes.put(ROOT_ID, new Node(ROOT_ID, null, "Fixture root", Document.MIME_TYPE_DIR, storage));
        try {
            String unicodeId = createDocument(ROOT_ID, "text/plain", "Grüße.txt");
            Files.write(nodes.get(unicodeId).file.toPath(), "hello\nworld\n".getBytes());
        } catch (IOException error) {
            throw new IllegalStateException(error);
        }
    }

    private static void deleteFileTree(File file) {
        File[] children = file.listFiles();
        if (children != null) for (File child : children) deleteFileTree(child);
        if (!file.delete() && file.exists()) throw new IllegalStateException("Could not clean fixture");
    }

    private static final class Node {
        final String id;
        String parentId;
        String name;
        final String mimeType;
        final File file;

        Node(String id, String parentId, String name, String mimeType, File file) {
            this.id = id;
            this.parentId = parentId;
            this.name = name;
            this.mimeType = mimeType;
            this.file = file;
        }

        boolean directory() {
            return Document.MIME_TYPE_DIR.equals(mimeType);
        }
    }
}
