// Simple client-side router with proper state management
const router = {
    currentRoute: '/',
    refreshTimeout: null,
    detailRefreshTimeout: null,
    currentInstanceId: null,
    initialized: false,
    renderInProgress: false,
    REFRESH_INTERVAL_MS: 3000, // Refresh every 3 seconds for live updates
    
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
            // Flash live indicator
            this.flashLiveIndicator();
            
            // Check if we already have a container for instances
            let instancesList = container.querySelector('.instances-list');
            const isInitialLoad = !instancesList;
            
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
                // Check if we already have empty state
                if (!container.querySelector('.empty-state')) {
                    container.innerHTML = components.emptyState(
                        'No running instances. Click "+ New Run" to start one.'
                    );
                }
            } else {
                console.log('[Router] Building HTML for', instances.length, 'instances');
                
                if (isInitialLoad) {
                    instancesList = document.createElement('div');
                    instancesList.className = 'instances-list';
                }
                
                // Build a map of existing panels for efficient lookup
                const existingPanels = new Map();
                if (!isInitialLoad) {
                    instancesList.querySelectorAll('.instance-panel').forEach(panel => {
                        const id = panel.getAttribute('data-id');
                        if (id) existingPanels.set(id, panel);
                    });
                }
                
                // Track which IDs we've seen
                const currentIds = new Set();
                
                for (const instance of instances) {
                    currentIds.add(instance.id);
                    const stats = instance.stats || { total_tokens: 0, tool_calls: 0, errors: 0, duration_secs: 0 };
                    const newHtml = components.instancePanel(instance, stats, instance.latest_message);
                    
                    const existingPanel = existingPanels.get(instance.id);
                    if (existingPanel) {
                        // Update existing panel in-place by replacing inner content
                        const tempDiv = document.createElement('div');
                        tempDiv.innerHTML = newHtml;
                        const newPanel = tempDiv.firstElementChild;
                        existingPanel.replaceWith(newPanel);
                    } else {
                        // Add new panel
                        const tempDiv = document.createElement('div');
                        tempDiv.innerHTML = newHtml;
                        instancesList.appendChild(tempDiv.firstElementChild);
                    }
                }
                
                // Remove panels for instances that no longer exist
                existingPanels.forEach((panel, id) => {
                    if (!currentIds.has(id)) {
                        panel.remove();
                    }
                });
                
                if (isInitialLoad) {
                    // Only clear if container doesn't already have instances-list
                    if (container.firstChild && container.firstChild !== instancesList) {
                        container.innerHTML = '';
                    }
                    container.appendChild(instancesList);
                }
                
                console.log('[Router] HTML set successfully');
            }
            
            // Schedule next refresh only if still on home route
            if (this.currentRoute === '/' || this.currentRoute === '') {
                console.log(`[Router] Scheduling auto-refresh in ${this.REFRESH_INTERVAL_MS}ms`);
                this.refreshTimeout = setTimeout(() => {
                    console.log('[Router] Auto-refresh triggered');
                    this.renderHome(container);
                }, this.REFRESH_INTERVAL_MS);
            }
        } catch (error) {
            console.error('[Router] Error in renderHome:', error);
            // Don't clear container on error, just show error message
            if (!container.querySelector('.error-message')) {
                const errorDiv = document.createElement('div');
                errorDiv.innerHTML = components.error('Failed to load instances: ' + error.message);
                container.appendChild(errorDiv.firstElementChild);
            }
        } finally {
            this.renderInProgress = false;
            console.log('[Router] renderHome complete, renderInProgress reset to false');
        }
    },
    
    flashLiveIndicator() {
        const indicator = document.getElementById('live-indicator');
        if (indicator) {
            indicator.style.animation = 'none';
            // Force reflow
            void indicator.offsetWidth;
            indicator.style.animation = null;
            indicator.style.opacity = '1';
        }
    },
    
    async renderDetail(container, id) {
        console.log('[Router] renderDetail called for', id);
        
        this.currentInstanceId = id;
        
        try {
            // Flash live indicator
            this.flashLiveIndicator();
            
            // Check if we already have a detail view for this instance
            let detailView = container.querySelector('.detail-view');
            const isInitialLoad = !detailView || detailView.getAttribute('data-instance-id') !== id;
            
            const instance = await api.getInstance(id);
            const logs = await api.getInstanceLogs(id);
            
            // Check if we're still on this detail route
            if (this.currentRoute !== `/instance/${id}`) {
                console.log('[Router] Route changed during fetch, aborting render');
                return;
            }
            
            // If not initial load, update in place
            if (!isInitialLoad) {
                detailView = container.querySelector('.detail-view');
                if (detailView) {
                    this.updateDetailView(detailView, instance, logs);
                    // Schedule next refresh
                    if (this.currentRoute === `/instance/${id}`) {
                        this.detailRefreshTimeout = setTimeout(() => {
                            this.renderDetail(container, id);
                        }, 3000);
                    }
                    return;
                }
            }
            
            // Build detail view HTML
            let html = `
                <div class="detail-view" data-instance-id="${id}">
                    <div class="detail-header">
                        <button class="btn btn-secondary" onclick="window.router.navigate('/')">&larr; Back</button>
                        <h2>${instance.workspace}</h2>
                        ${components.statusBadge(instance.status)}
                    </div>
                    
                    <div class="detail-stats">
                        <div class="stat-card" data-stat="tokens">
                            <div class="stat-label">Tokens</div>
                            <div class="stat-value">${(instance.stats?.total_tokens || 0).toLocaleString()}</div>
                        </div>
                        <div class="stat-card" data-stat="tool_calls">
                            <div class="stat-label">Tool Calls</div>
                            <div class="stat-value">${instance.stats?.tool_calls || 0}</div>
                        </div>
                        <div class="stat-card" data-stat="errors">
                            <div class="stat-label">Errors</div>
                            <div class="stat-value">${instance.stats?.errors || 0}</div>
                        </div>
                        <div class="stat-card" data-stat="duration">
                            <div class="stat-label">Duration</div>
                            <div class="stat-value">${Math.round((instance.stats?.duration_secs || 0) / 60)}m</div>
                        </div>
                    </div>
                    
                    <div class="detail-section">
                        <h3>Git Status</h3>
                        <div class="git-status-container">${components.gitStatus(instance.git_status)}</div>
                    </div>
                    
                    <div class="detail-section">
                        <h3>Project Files</h3>
                        <div class="project-files-container">${components.projectFiles(instance.project_files)}</div>
                    </div>
                    
                    <div class="detail-content">
                        <h3>Tool Calls</h3>
                        <div class="tool-calls-section" data-section="tool-calls">
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
            // Don't clear container on error, just show error message
            if (!container.querySelector('.error-message')) {
                const errorDiv = document.createElement('div');
                errorDiv.innerHTML = components.error('Failed to load instance: ' + error.message);
                container.appendChild(errorDiv.firstElementChild);
            }
        }
    },
    
    updateDetailView(detailView, instance, logs) {
        // Update status badge
        const statusBadge = detailView.querySelector('.detail-header .badge');
        if (statusBadge) {
            const tempDiv = document.createElement('div');
            tempDiv.innerHTML = components.statusBadge(instance.status);
            statusBadge.replaceWith(tempDiv.firstElementChild);
        }
        
        // Update stats
        const tokensStat = detailView.querySelector('[data-stat="tokens"] .stat-value');
        if (tokensStat) {
            tokensStat.textContent = (instance.stats?.total_tokens || 0).toLocaleString();
        }
        
        const toolCallsStat = detailView.querySelector('[data-stat="tool_calls"] .stat-value');
        if (toolCallsStat) {
            toolCallsStat.textContent = instance.stats?.tool_calls || 0;
        }
        
        const errorsStat = detailView.querySelector('[data-stat="errors"] .stat-value');
        if (errorsStat) {
            errorsStat.textContent = instance.stats?.errors || 0;
        }
        
        const durationStat = detailView.querySelector('[data-stat="duration"] .stat-value');
        if (durationStat) {
            durationStat.textContent = Math.round((instance.stats?.duration_secs || 0) / 60) + 'm';
        }
        
        // Update git status
        const gitStatusContainer = detailView.querySelector('.git-status-container');
        if (gitStatusContainer) {
            gitStatusContainer.innerHTML = components.gitStatus(instance.git_status);
        }
        
        // Update project files
        const projectFilesContainer = detailView.querySelector('.project-files-container');
        if (projectFilesContainer) {
            projectFilesContainer.innerHTML = components.projectFiles(instance.project_files);
        }
        
        // Update tool calls
        const toolCallsSection = detailView.querySelector('[data-section="tool-calls"]');
        if (toolCallsSection && logs && logs.tool_calls) {
            // Build a map of existing tool calls
            const existingToolCalls = new Map();
            toolCallsSection.querySelectorAll('.tool-call').forEach(tc => {
                const id = tc.getAttribute('data-tool-id');
                if (id) existingToolCalls.set(id, tc);
            });
            
            // Track which IDs we've seen
            const currentIds = new Set();
            
            if (logs.tool_calls.length > 0) {
                for (const toolCall of logs.tool_calls) {
                    currentIds.add(toolCall.id);
                    const newHtml = components.toolCall(toolCall);
                    
                    const existingToolCall = existingToolCalls.get(toolCall.id);
                    if (existingToolCall) {
                        // Update existing tool call in-place
                        const tempDiv = document.createElement('div');
                        tempDiv.innerHTML = newHtml;
                        existingToolCall.replaceWith(tempDiv.firstElementChild);
                    } else {
                        // Add new tool call
                        const tempDiv = document.createElement('div');
                        tempDiv.innerHTML = newHtml;
                        toolCallsSection.appendChild(tempDiv.firstElementChild);
                    }
                }
                
                // Remove tool calls that no longer exist
                existingToolCalls.forEach((tc, id) => {
                    if (!currentIds.has(id)) {
                        tc.remove();
                    }
                });
            }
        }
        
        // Update chat messages
        const chatMessages = detailView.querySelector('.chat-messages');
        if (chatMessages && logs && logs.messages && logs.messages.length > 0) {
            let html = '';
            for (const msg of logs.messages) {
                html += components.chatMessage(msg.content, msg.agent);
            }
            chatMessages.innerHTML = html;
        }
        
        // Re-apply syntax highlighting to any new code blocks
        detailView.querySelectorAll('pre code:not(.hljs)').forEach((block) => {
            hljs.highlightElement(block);
        });
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
