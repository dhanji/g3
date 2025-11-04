// Simple client-side router
const router = {
    currentRoute: '/',
    
    init() {
        // Handle browser back/forward
        window.addEventListener('popstate', () => {
            this.handleRoute(window.location.pathname);
        });
        
        // Handle initial route
        this.handleRoute(window.location.pathname);
    },
    
    navigate(path) {
        window.history.pushState({}, '', path);
        this.handleRoute(path);
    },
    
    async handleRoute(path) {
        this.currentRoute = path;
        const container = document.getElementById('page-container');
        
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
        container.innerHTML = components.spinner('Loading instances...');
        
        try {
            const instances = await api.getInstances();
            
            if (instances.length === 0) {
                container.innerHTML = components.emptyState(
                    'No running instances. Click "+ New Run" to start one.'
                );
                return;
            }
            
            let html = '<div class="instances-list">';
            for (const instance of instances) {
                // Use stats from API response
                const stats = instance.stats || { total_tokens: 0, tool_calls: 0, errors: 0, duration_secs: 0 };
                html += components.instancePanel(instance, stats, instance.latest_message);
            }
            html += '</div>';
            
            container.innerHTML = html;
            
            // Auto-refresh every 5 seconds
            setTimeout(() => {
                if (this.currentRoute === '/') {
                    this.renderHome(container);
                }
            }, 5000);
        } catch (error) {
            container.innerHTML = components.error(error.message);
        }
    },
    
    async renderDetail(container, id) {
        container.innerHTML = components.spinner('Loading instance details...');
        
        try {
            const instance = await api.getInstance(id);
            const logs = await api.getInstanceLogs(id);
            
            // Build detail view HTML
            let html = `
                <div class="detail-view">
                    <div class="detail-header">
                        <button class="btn btn-secondary" onclick="router.navigate('/')">&larr; Back</button>
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
            
            // Auto-refresh every 3 seconds
            setTimeout(() => {
                if (this.currentRoute === `/instance/${id}`) {
                    this.renderDetail(container, id);
                }
            }, 3000);
        } catch (error) {
            container.innerHTML = components.error(error.message);
        }
    }
};
