// Simple client-side router with proper state management
const router = {
    currentRoute: '/',
    refreshTimeout: null,
    detailRefreshTimeout: null,
    currentInstanceId: null,
    initialized: false,
    renderInProgress: false,
    
    init() {
        console.log('[Router] init() called');
        if (this.initialized) {
            console.log('[Router] Already initialized, skipping');
            return;
        }
        this.initialized = true;
        
        // Handle browser back/forward
        window.addEventListener('popstate', () => {
            console.log('[Router] popstate event');
            this.handleRoute(window.location.pathname);
        });
        
        // Handle initial route - call once after a short delay to ensure DOM is ready
        setTimeout(() => {
            console.log('[Router] Initial route handling');
            this.handleRoute(window.location.pathname);
        }, 100);
    },
    
    navigate(path) {
        console.log('[Router] navigate:', path);
        // Cancel any pending refreshes
        this.cancelRefreshes();
        window.history.pushState({}, '', path);
        this.handleRoute(path);
    },
    
    cancelRefreshes() {
        if (this.refreshTimeout) {
            console.log('[Router] Cancelling home refresh timeout');
            clearTimeout(this.refreshTimeout);
            this.refreshTimeout = null;
        }
        if (this.detailRefreshTimeout) {
            console.log('[Router] Cancelling detail refresh timeout');
            clearTimeout(this.detailRefreshTimeout);
            this.detailRefreshTimeout = null;
        }
    },
    
    async handleRoute(path) {
        this.currentRoute = path;
        console.log('[Router] handleRoute:', path);
        const container = document.getElementById('page-container');
        
        if (!container) {
            console.error('[Router] page-container not found!');
            return;
        }
        
        // Cancel any pending refreshes when route changes
        this.cancelRefreshes();
        
        if (path === '/' || path === '') {
            await this.renderHome(container);
        } else if (path.startsWith('/instance/')) {
            const id = path.split('/')[2];
            await this.renderDetail(container, id);
        } else {
            container.innerHTML = components.error('Page not found');
        }
    },
    
    async renderHome(container) {
        console.log('[Router] renderHome called, renderInProgress:', this.renderInProgress);
        
        // Prevent concurrent renders
        if (this.renderInProgress) {
            console.log('[Router] Render already in progress, skipping');
            return;
        }
        
        this.renderInProgress = true;
        
        try {
            console.log('[Router] Showing spinner');
            container.innerHTML = components.spinner('Loading instances...');
            
            console.log('[Router] Fetching instances from API');
            const instances = await api.getInstances();
            console.log('[Router] Received', instances.length, 'instances');
            
            // Check if we're still on the home route (user might have navigated away)
            if (this.currentRoute !== '/' && this.currentRoute !== '') {
                console.log('[Router] Route changed during fetch, aborting render');
                return;
            }
            
            if (instances.length === 0) {
                console.log('[Router] No instances, showing empty state');
                container.innerHTML = components.emptyState(
                    'No running instances. Click "+ New Run" to start one.'
                );
            } else {
                console.log('[Router] Building HTML for', instances.length, 'instances');
                let html = '<div class="instances-list">';
                for (const instance of instances) {
                    const stats = instance.stats || { total_tokens: 0, tool_calls: 0, errors: 0, duration_secs: 0 };
                    html += components.instancePanel(instance, stats, instance.latest_message);
                }
                html += '</div>';
                
                console.log('[Router] Setting innerHTML (', html.length, 'chars)');
                container.innerHTML = html;
                console.log('[Router] HTML set successfully');
            }
            
            // Schedule next refresh only if still on home route
            if (this.currentRoute === '/' || this.currentRoute === '') {
                console.log('[Router] Scheduling auto-refresh in 5 seconds');
                this.refreshTimeout = setTimeout(() => {
                    console.log('[Router] Auto-refresh triggered');
                    this.renderHome(container);
                }, 5000);
            }
        } catch (error) {
            console.error('[Router] Error in renderHome:', error);
            container.innerHTML = components.error('Failed to load instances: ' + error.message);
        } finally {
            this.renderInProgress = false;
            console.log('[Router] renderHome complete, renderInProgress reset to false');
        }
    },
    
    async renderDetail(container, id) {
        console.log('[Router] renderDetail called for', id);
        
        this.currentInstanceId = id;
        container.innerHTML = components.spinner('Loading instance details...');
        
        try {
            const instance = await api.getInstance(id);
            const logs = await api.getInstanceLogs(id);
            
            // Check if we're still on this detail route
            if (this.currentRoute !== `/instance/${id}`) {
                console.log('[Router] Route changed during fetch, aborting render');
                return;
            }
            
            // Build detail view HTML
            let html = `
                <div class="detail-view">
                    <div class="detail-header">
                        <button class="btn btn-secondary" onclick="window.router.navigate('/')">&larr; Back</button>
                        <h2>${instance.workspace}</h2>
                        ${components.statusBadge(instance.status)}
                    </div>
                    
                    <div class="detail-stats">
                        <div class="stat-card">
                            <div class="stat-label">Tokens</div>
                            <div class="stat-value">${(instance.stats?.total_tokens || 0).toLocaleString()}</div>
                        </div>
                        <div class="stat-card">
                            <div class="stat-label">Tool Calls</div>
                            <div class="stat-value">${instance.stats?.tool_calls || 0}</div>
                        </div>
                        <div class="stat-card">
                            <div class="stat-label">Errors</div>
                            <div class="stat-value">${instance.stats?.errors || 0}</div>
                        </div>
                        <div class="stat-card">
                            <div class="stat-label">Duration</div>
                            <div class="stat-value">${Math.round((instance.stats?.duration_secs || 0) / 60)}m</div>
                        </div>
                    </div>
                    
                    <div class="detail-section">
                        <h3>Git Status</h3>
                        ${components.gitStatus(instance.git_status)}
                    </div>
                    
                    <div class="detail-section">
                        <h3>Project Files</h3>
                        ${components.projectFiles(instance.project_files)}
                    </div>
                    
                    <div class="detail-content">
                        <h3>Tool Calls</h3>
                        <div class="tool-calls-section">
            `;
            
            // Render tool calls
            if (logs && logs.tool_calls && logs.tool_calls.length > 0) {
                for (const toolCall of logs.tool_calls) {
                    html += components.toolCall(toolCall);
                }
            } else {
                html += '<p class="text-muted">No tool calls yet</p>';
            }
            
            html += `
                        </div>
                        
                        <h3>Chat History</h3>
                        <div class="chat-messages">
            `;
            
            // Render messages from logs
            if (logs && logs.messages && logs.messages.length > 0) {
                for (const msg of logs.messages) {
                    html += components.chatMessage(msg.content, msg.agent);
                }
            } else {
                html += '<p class="text-muted">No messages yet</p>';
            }
            
            html += `
                            </div>
                        </div>
                    </div>
                </div>
            `;
            
            container.innerHTML = html;
            
            // Apply syntax highlighting
            document.querySelectorAll('pre code').forEach((block) => {
                hljs.highlightElement(block);
            });
            
            // Schedule next refresh only if still on this detail route
            if (this.currentRoute === `/instance/${id}`) {
                this.detailRefreshTimeout = setTimeout(() => {
                    this.renderDetail(container, id);
                }, 3000);
            }
        } catch (error) {
            console.error('[Router] Error in renderDetail:', error);
            container.innerHTML = components.error('Failed to load instance: ' + error.message);
        }
    }
};

// Global function to view full file content
window.viewFullFile = async function(fileName) {
    const modal = document.getElementById('full-file-modal');
    const title = document.getElementById('full-file-title');
    const content = document.getElementById('full-file-content');
    
    // Show modal
    modal.classList.remove('hidden');
    title.textContent = fileName;
    content.innerHTML = '<div class="spinner-container"><div class="spinner"></div><p>Loading...</p></div>';
    
    try {
        const instanceId = window.router.currentInstanceId;
        if (!instanceId) {
            throw new Error('No instance selected');
        }
        
        const data = await api.getFileContent(instanceId, fileName);
        
        // Render full content with syntax highlighting
        content.innerHTML = `<pre><code class="language-markdown">${components.escapeHtml(data.content)}</code></pre>`;
        
        // Apply syntax highlighting
        content.querySelectorAll('pre code').forEach((block) => {
            hljs.highlightElement(block);
        });
    } catch (error) {
        content.innerHTML = `<div class="error-message">Failed to load file: ${error.message}</div>`;
    }
};

// Close full file modal
document.addEventListener('DOMContentLoaded', () => {
    document.getElementById('full-file-close')?.addEventListener('click', () => {
        document.getElementById('full-file-modal').classList.add('hidden');
    });
});

// Expose to window for global access
window.router = router;
