// UI Components for G3 Console

const components = {
    // Render status badge
    statusBadge(status) {
        const colors = {
            running: 'badge-success',
            completed: 'badge-success',
            failed: 'badge-error',
            idle: 'badge-warning',
            terminated: 'badge-neutral'
        };
        return `<span class="badge ${colors[status] || 'badge-neutral'}">${status}</span>`;
    },

    // Render progress bar
    progressBar(instance, stats) {
        const duration = stats.duration_secs;
        
        // Handle zero duration to avoid NaN
        if (duration === 0) {
            return this.singleProgressBar(0);
        }
        
        const estimated = duration * 1.5; // Simple estimation
        const progress = Math.min((duration / estimated) * 100, 100);
        
        // Check if this is ensemble mode with turn data
        if (instance.instance_type === 'ensemble' && stats.turns && stats.turns.length > 0) {
            return this.ensembleProgressBar(stats.turns, duration);
        }
        
        return `
            <div class="progress-bar">
                <div class="progress-fill" style="width: ${progress}%"></div>
                <span class="progress-text">${Math.round(duration / 60)}m elapsed</span>
            </div>
        `;
    },

    // Render multi-segment progress bar for ensemble mode
    ensembleProgressBar(turns, totalDuration) {
        const colors = {
            coach: '#3b82f6',
            player: '#6b7280',
            completed: '#10b981',
            error: '#ef4444'
        };
        
        if (turns.length === 0) {
            // Fallback to single progress bar if no turn data
            return this.singleProgressBar(totalDuration);
        }
        
        let segments = '';
        for (const turn of turns) {
            // Handle zero total duration to avoid NaN
            if (totalDuration === 0) {
                continue;
            }
            
            // Ensure percentage never exceeds 100%
            const rawPercentage = (turn.duration_secs / totalDuration) * 100;
            const percentage = Math.min(rawPercentage, 100);
            const color = colors[turn.agent] || colors.player;
            const statusColor = turn.status === 'error' ? colors.error : color;
            const agentLabel = turn.agent.charAt(0).toUpperCase() + turn.agent.slice(1);
            const durationMin = Math.round(turn.duration_secs / 60);
            const tooltip = `${agentLabel}: ${durationMin}m ${Math.round(turn.duration_secs % 60)}s - ${turn.status}`;
            
            segments += `
                <div class="progress-segment" 
                     style="width: ${percentage}%; background-color: ${statusColor};"
                     title="${tooltip}">
                </div>
            `;
        }
        
        return `
            <div class="progress-bar ensemble">
                ${segments}
                <span class="progress-text">${Math.round(totalDuration / 60)}m elapsed</span>
            </div>
        `;
    },
    
    // Single progress bar (fallback)
    singleProgressBar(duration) {
        // Handle zero duration
        if (duration === 0) {
            return `<div class="progress-bar"><div class="progress-fill" style="width: 0%"></div><span class="progress-text">Starting...</span></div>`;
        }
        
        const estimated = duration * 1.5;
        const progress = Math.min((duration / estimated) * 100, 100);
        return `
            <div class="progress-bar">
                <div class="progress-fill" style="width: ${progress}%"></div>
                <span class="progress-text">${Math.round(duration / 60)}m elapsed</span>
            </div>
        `;
    },

    // Render instance panel
    instancePanel(instance, stats, latestMessage) {
        return `
            <div class="instance-panel" data-id="${instance.id}" onclick="event.preventDefault(); event.stopPropagation(); window.router.navigate('/instance/${instance.id}')">
                <div class="panel-header">
                    <div class="panel-title">
                        <h3>${instance.workspace}</h3>
                        ${this.statusBadge(instance.status)}
                    </div>
                    <div class="panel-meta">
                        <span class="meta-item">${instance.instance_type}</span>
                        <span class="meta-item">PID: ${instance.pid}</span>
                        <span class="meta-item">${new Date(instance.start_time).toLocaleString()}</span>
                    </div>
                </div>
                
                ${this.progressBar(instance, stats)}
                
                <div class="panel-stats">
                    <div class="stat-item">
                        <span class="stat-label">Tokens</span>
                        <span class="stat-value">${stats.total_tokens.toLocaleString()}</span>
                    </div>
                    <div class="stat-item">
                        <span class="stat-label">Tool Calls</span>
                        <span class="stat-value">${stats.tool_calls}</span>
                    </div>
                    <div class="stat-item">
                        <span class="stat-label">Errors</span>
                        <span class="stat-value">${stats.errors}</span>
                    </div>
                    <div class="stat-item">
                        <span class="stat-label">Duration</span>
                        <span class="stat-value">${Math.round(stats.duration_secs / 60)}m</span>
                    </div>
                </div>
                
                ${latestMessage ? `
                    <div class="panel-message">
                        <strong>Latest:</strong> ${this.truncate(latestMessage, 100)}
                    </div>
                ` : ''}
                
                <div class="panel-actions">
                    ${instance.status === 'running' ? `
                        <button class="btn btn-danger btn-sm" onclick="event.stopPropagation(); handleKill('${instance.id}')">Kill</button>
                    ` : ''}
                    ${instance.status === 'terminated' ? `
                        <button class="btn btn-primary btn-sm" onclick="event.stopPropagation(); handleRestart('${instance.id}')">Restart</button>
                    ` : ''}
                    <button class="btn btn-secondary btn-sm" onclick="event.stopPropagation(); router.navigate('/instance/${instance.id}')">View Details</button>
                </div>
            </div>
        `;
    },

    // Render loading spinner
    spinner(message = 'Loading...') {
        return `
            <div class="spinner-container">
                <div class="spinner"></div>
                <p>${message}</p>
            </div>
        `;
    },

    // Render error message
    error(message) {
        return `
            <div class="error-message">
                <strong>Error:</strong> ${message}
            </div>
        `;
    },

    // Render empty state
    emptyState(message) {
        return `
            <div class="empty-state">
                <p>${message}</p>
            </div>
        `;
    },

    // Truncate text
    truncate(text, length) {
        if (text.length <= length) return text;
        return text.substring(0, length) + '...';
    },

    // Render chat message
    chatMessage(message, agent = null) {
        // Handle agent as string or object
        let agentStr = null;
        if (typeof agent === 'string') {
            agentStr = agent.toLowerCase();
        } else if (agent && typeof agent === 'object') {
            agentStr = String(agent).toLowerCase();
        }
        
        const agentClass = agentStr === 'coach' ? 'message-coach' : agentStr === 'player' ? 'message-player' : '';
        
        return `
            <div class="chat-message ${agentClass}">
                ${agentStr ? `<div class="message-agent">${agentStr}</div>` : ''}
                <div class="message-content">${marked.parse(message)}</div>
            </div>
        `;
    },

    // Render tool call
    toolCall(toolCall) {
        const statusIcon = toolCall.success ? '✓' : '✗';
        const statusClass = toolCall.success ? 'success' : 'error';
        
        return `
            <div class="tool-call" data-tool-id="${toolCall.id}">
                <div class="tool-header" onclick="this.parentElement.classList.toggle('expanded')">
                    <span class="tool-name">🔧 ${toolCall.tool_name}</span>
                    <div class="tool-header-right">
                        ${toolCall.execution_time_ms ? `<span class="tool-time">${toolCall.execution_time_ms}ms</span>` : ''}
                        <span class="tool-status ${statusClass}">${statusIcon}</span>
                    </div>
                </div>
                <div class="tool-details">
                    <div class="tool-section">
                        <strong>Parameters:</strong>
                        <pre><code class="language-json">${JSON.stringify(toolCall.parameters, null, 2)}</code></pre>
                    </div>
                    ${toolCall.result ? `
                        <div class="tool-section">
                            <strong>Result:</strong>
                            <pre><code class="language-json">${JSON.stringify(toolCall.result, null, 2)}</code></pre>
                        </div>
                    ` : ''}
                    ${toolCall.error ? `
                        <div class="tool-section">
                            <strong>Error:</strong>
                            <pre><code class="language-text">${this.escapeHtml(toolCall.error)}</code></pre>
                        </div>
                    ` : ''}
                    <div class="tool-meta">
                        <span>Timestamp: ${new Date(toolCall.timestamp).toLocaleString()}</span>
                        ${toolCall.execution_time_ms ? `<span> • Duration: ${toolCall.execution_time_ms}ms</span>` : ''}
                        <span> • Status: ${toolCall.success ? 'Success' : 'Failed'}</span>
                    </div>
                </div>
            </div>
        `;
    },

    // Render git status section
    gitStatus(gitStatus) {
        if (!gitStatus) {
            return '<p class="text-muted">No git repository detected</p>';
        }
        
        return `
            <div class="git-status">
                <div class="git-header">
                    <span class="git-branch">📍 ${gitStatus.branch}</span>
                    <span class="git-changes">${gitStatus.uncommitted_changes} uncommitted changes</span>
                </div>
                ${gitStatus.uncommitted_changes > 0 ? `
                    <div class="git-files">
                        ${gitStatus.modified_files.length > 0 ? `
                            <div class="git-file-group">
                                <strong class="file-status modified">Modified:</strong>
                                <ul>
                                    ${gitStatus.modified_files.map(f => `<li>${f}</li>`).join('')}
                                </ul>
                            </div>
                        ` : ''}
                        ${gitStatus.added_files.length > 0 ? `
                            <div class="git-file-group">
                                <strong class="file-status added">Added:</strong>
                                <ul>
                                    ${gitStatus.added_files.map(f => `<li>${f}</li>`).join('')}
                                </ul>
                            </div>
                        ` : ''}
                        ${gitStatus.deleted_files.length > 0 ? `
                            <div class="git-file-group">
                                <strong class="file-status deleted">Deleted:</strong>
                                <ul>
                                    ${gitStatus.deleted_files.map(f => `<li>${f}</li>`).join('')}
                                </ul>
                            </div>
                        ` : ''}
                    </div>
                ` : ''}
            </div>
        `;
    },

    // Render project files section
    projectFiles(projectFiles) {
        if (!projectFiles || (!projectFiles.requirements && !projectFiles.readme && !projectFiles.agents)) {
            return '<p class="text-muted">No project files found</p>';
        }
        
        let html = '<div class="project-files">';
        
        if (projectFiles.requirements) {
            html += `
                <div class="project-file">
                    <div class="file-header" onclick="this.parentElement.classList.toggle('expanded')">
                        <span class="file-name">📄 requirements.md</span>
                        <button class="btn btn-sm btn-secondary" onclick="event.stopPropagation(); window.viewFullFile('requirements.md')" style="margin-left: auto; margin-right: 0.5rem;">View Full</button>
                        <span class="file-toggle">▼</span>
                    </div>
                    <div class="file-content">
                        <pre><code>${this.escapeHtml(projectFiles.requirements)}</code></pre>
                        <p class="text-muted" style="margin-top: 0.5rem; font-size: 0.875rem;">Showing first 10 lines...</p>
                    </div>
                </div>
            `;
        }
        
        if (projectFiles.readme) {
            html += `
                <div class="project-file">
                    <div class="file-header" onclick="this.parentElement.classList.toggle('expanded')">
                        <span class="file-name">📄 README.md</span>
                        <button class="btn btn-sm btn-secondary" onclick="event.stopPropagation(); window.viewFullFile('README.md')" style="margin-left: auto; margin-right: 0.5rem;">View Full</button>
                        <span class="file-toggle">▼</span>
                    </div>
                    <div class="file-content">
                        <pre><code>${this.escapeHtml(projectFiles.readme)}</code></pre>
                        <p class="text-muted" style="margin-top: 0.5rem; font-size: 0.875rem;">Showing first 10 lines...</p>
                    </div>
                </div>
            `;
        }
        
        if (projectFiles.agents) {
            html += `
                <div class="project-file">
                    <div class="file-header" onclick="this.parentElement.classList.toggle('expanded')">
                        <span class="file-name">📄 AGENTS.md</span>
                        <button class="btn btn-sm btn-secondary" onclick="event.stopPropagation(); window.viewFullFile('AGENTS.md')" style="margin-left: auto; margin-right: 0.5rem;">View Full</button>
                        <span class="file-toggle">▼</span>
                    </div>
                    <div class="file-content">
                        <pre><code>${this.escapeHtml(projectFiles.agents)}</code></pre>
                        <p class="text-muted" style="margin-top: 0.5rem; font-size: 0.875rem;">Showing first 10 lines...</p>
                    </div>
                </div>
            `;
        }
        
        html += '</div>';
        return html;
    },

    escapeHtml(text) {
        const div = document.createElement('div');
        div.textContent = text;
        return div.innerHTML;
    }
};

// Expose to window for global access
window.components = components;
