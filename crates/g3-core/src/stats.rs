//! Agent statistics formatting module.
//!
//! This module provides functionality for formatting detailed statistics
//! about the agent's context window, performance, and tool usage.

use g3_providers::MessageRole;
use std::time::Duration;

use crate::context_window::ContextWindow;
use crate::CacheStats;

/// Data required to format agent statistics.
/// This struct captures a snapshot of agent state for formatting.
pub struct AgentStatsSnapshot<'a> {
    pub context_window: &'a ContextWindow,
    pub thinning_events: &'a [usize],
    pub compaction_events: &'a [usize],
    pub first_token_times: &'a [Duration],
    pub tool_call_metrics: &'a [(String, Duration, bool)],
    pub provider_info: Option<(String, String)>,
    pub cache_stats: &'a CacheStats,
}

impl<'a> AgentStatsSnapshot<'a> {
    /// Format detailed context statistics as a string.
    pub fn format(&self) -> String {
        let mut stats = String::new();

        stats.push_str("\n📊 Context Window Statistics\n");
        stats.push_str(&"=".repeat(60));
        stats.push_str("\n\n");

        self.format_context_window(&mut stats);
        self.format_optimization_metrics(&mut stats);
        self.format_performance_metrics(&mut stats);
        self.format_conversation_history(&mut stats);
        self.format_tool_call_metrics(&mut stats);
        self.format_cache_stats(&mut stats);
        self.format_provider_info(&mut stats);

        stats.push_str(&"=".repeat(60));
        stats.push('\n');

        stats
    }

    fn format_context_window(&self, stats: &mut String) {
        stats.push_str("🗂️  Context Window:\n");
        stats.push_str(&format!(
            "   • Used Tokens:       {:>10} / {}\n",
            self.context_window.used_tokens, self.context_window.total_tokens
        ));
        stats.push_str(&format!(
            "   • Usage Percentage:  {:>10.1}%\n",
            self.context_window.percentage_used()
        ));
        stats.push_str(&format!(
            "   • Remaining Tokens:  {:>10}\n",
            self.context_window.remaining_tokens()
        ));
        stats.push_str(&format!(
            "   • Cumulative Tokens: {:>10}\n",
            self.context_window.cumulative_tokens
        ));
        stats.push_str(&format!(
            "   • Last Thinning:     {:>10}%\n",
            self.context_window.last_thinning_percentage
        ));
        stats.push('\n');
    }

    fn format_optimization_metrics(&self, stats: &mut String) {
        stats.push_str("🗜️  Context Optimization:\n");
        stats.push_str(&format!(
            "   • Thinning Events:   {:>10}\n",
            self.thinning_events.len()
        ));
        if !self.thinning_events.is_empty() {
            let total_thinned: usize = self.thinning_events.iter().sum();
            let avg_thinned = total_thinned / self.thinning_events.len();
            stats.push_str(&format!("   • Total Chars Saved: {:>10}\n", total_thinned));
            stats.push_str(&format!("   • Avg Chars/Event:   {:>10}\n", avg_thinned));
        }

        stats.push_str(&format!(
            "   • Compactions:       {:>10}\n",
            self.compaction_events.len()
        ));
        if !self.compaction_events.is_empty() {
            let total_compacted: usize = self.compaction_events.iter().sum();
            let avg_compacted = total_compacted / self.compaction_events.len();
            stats.push_str(&format!(
                "   • Total Chars Saved: {:>10}\n",
                total_compacted
            ));
            stats.push_str(&format!("   • Avg Chars/Event:   {:>10}\n", avg_compacted));
        }
        stats.push('\n');
    }

    fn format_performance_metrics(&self, stats: &mut String) {
        stats.push_str("⚡ Performance:\n");
        if !self.first_token_times.is_empty() {
            let avg_ttft = self.first_token_times.iter().sum::<Duration>()
                / self.first_token_times.len() as u32;
            let mut sorted_times = self.first_token_times.to_vec();
            sorted_times.sort();
            let median_ttft = sorted_times[sorted_times.len() / 2];
            stats.push_str(&format!(
                "   • Avg Time to First Token:    {:>6.3}s\n",
                avg_ttft.as_secs_f64()
            ));
            stats.push_str(&format!(
                "   • Median Time to First Token: {:>6.3}s\n",
                median_ttft.as_secs_f64()
            ));
        }
        stats.push('\n');
    }

    fn format_conversation_history(&self, stats: &mut String) {
        stats.push_str("💬 Conversation History:\n");
        stats.push_str(&format!(
            "   • Total Messages:    {:>10}\n",
            self.context_window.conversation_history.len()
        ));

        // Count messages by role
        let mut system_count = 0;
        let mut user_count = 0;
        let mut assistant_count = 0;

        for msg in &self.context_window.conversation_history {
            match msg.role {
                MessageRole::System => system_count += 1,
                MessageRole::User => user_count += 1,
                MessageRole::Assistant => assistant_count += 1,
            }
        }

        stats.push_str(&format!("   • System Messages:   {:>10}\n", system_count));
        stats.push_str(&format!("   • User Messages:     {:>10}\n", user_count));
        stats.push_str(&format!(
            "   • Assistant Messages:{:>10}\n",
            assistant_count
        ));
        stats.push('\n');
    }

    fn format_tool_call_metrics(&self, stats: &mut String) {
        stats.push_str("🔧 Tool Call Metrics:\n");
        stats.push_str(&format!(
            "   • Total Tool Calls:  {:>10}\n",
            self.tool_call_metrics.len()
        ));

        let successful_calls = self
            .tool_call_metrics
            .iter()
            .filter(|(_, _, success)| *success)
            .count();
        let failed_calls = self.tool_call_metrics.len() - successful_calls;

        stats.push_str(&format!(
            "   • Successful:        {:>10}\n",
            successful_calls
        ));
        stats.push_str(&format!("   • Failed:            {:>10}\n", failed_calls));

        if !self.tool_call_metrics.is_empty() {
            let total_duration: Duration = self
                .tool_call_metrics
                .iter()
                .map(|(_, duration, _)| *duration)
                .sum();
            let avg_duration = total_duration / self.tool_call_metrics.len() as u32;

            stats.push_str(&format!(
                "   • Total Duration:    {:>10.2}s\n",
                total_duration.as_secs_f64()
            ));
            stats.push_str(&format!(
                "   • Average Duration:  {:>10.2}s\n",
                avg_duration.as_secs_f64()
            ));
        }
        stats.push('\n');
    }

    fn format_cache_stats(&self, stats: &mut String) {
        stats.push_str("💾 Prompt Cache Statistics:\n");
        stats.push_str(&format!(
            "   • API Calls:         {:>10}\n",
            self.cache_stats.total_calls
        ));
        stats.push_str(&format!(
            "   • Cache Hits:        {:>10}\n",
            self.cache_stats.cache_hit_calls
        ));
        
        // Calculate hit rate
        let hit_rate = if self.cache_stats.total_calls > 0 {
            (self.cache_stats.cache_hit_calls as f64 / self.cache_stats.total_calls as f64) * 100.0
        } else {
            0.0
        };
        stats.push_str(&format!("   • Hit Rate:          {:>9.1}%\n", hit_rate));
        
        stats.push_str(&format!(
            "   • Total Input Tokens:{:>10}\n",
            self.cache_stats.total_input_tokens
        ));
        stats.push_str(&format!(
            "   • Total Output Tokens:{:>9}\n",
            self.cache_stats.total_output_tokens
        ));
        stats.push_str(&format!(
            "   • Cache Created:     {:>10} tokens\n",
            self.cache_stats.total_cache_creation_tokens
        ));
        stats.push_str(&format!(
            "   • Cache Read:        {:>10} tokens\n",
            self.cache_stats.total_cache_read_tokens
        ));
        
        // Calculate cache read percentage of total input
        let cache_read_pct = if self.cache_stats.total_input_tokens > 0 {
            (self.cache_stats.total_cache_read_tokens as f64
                / self.cache_stats.total_input_tokens as f64)
                * 100.0
        } else {
            0.0
        };
        stats.push_str(&format!(
            "   • Cache Efficiency:  {:>9.1}% of input from cache\n",
            cache_read_pct
        ));
        stats.push('\n');
    }

    fn format_provider_info(&self, stats: &mut String) {
        stats.push_str("🔌 Provider:\n");
        if let Some((provider, model)) = &self.provider_info {
            stats.push_str(&format!("   • Provider:          {}\n", provider));
            stats.push_str(&format!("   • Model:             {}\n", model));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context_window::ContextWindow;

    #[test]
    fn test_format_stats_empty() {
        let context_window = ContextWindow::new(100000);
        let cache_stats = CacheStats::default();
        let snapshot = AgentStatsSnapshot {
            context_window: &context_window,
            thinning_events: &[],
            compaction_events: &[],
            first_token_times: &[],
            tool_call_metrics: &[],
            provider_info: None,
            cache_stats: &cache_stats,
        };

        let stats = snapshot.format();
        assert!(stats.contains("Context Window Statistics"));
        assert!(stats.contains("Used Tokens"));
        assert!(stats.contains("Thinning Events"));
        assert!(stats.contains("Tool Call Metrics"));
        assert!(stats.contains("Prompt Cache Statistics"));
    }

    #[test]
    fn test_format_stats_with_data() {
        let context_window = ContextWindow::new(100000);
        let thinning_events = vec![1000, 2000, 1500];
        let compaction_events = vec![5000];
        let cache_stats = CacheStats {
            total_calls: 5,
            cache_hit_calls: 3,
            total_input_tokens: 10000,
            total_cache_creation_tokens: 2000,
            total_cache_read_tokens: 6000,
            ..Default::default()
        };
        let first_token_times = vec![
            Duration::from_millis(100),
            Duration::from_millis(150),
            Duration::from_millis(120),
        ];
        let tool_call_metrics = vec![
            ("shell".to_string(), Duration::from_millis(500), true),
            ("read_file".to_string(), Duration::from_millis(100), true),
            ("write_file".to_string(), Duration::from_millis(200), false),
        ];

        let snapshot = AgentStatsSnapshot {
            context_window: &context_window,
            thinning_events: &thinning_events,
            compaction_events: &compaction_events,
            first_token_times: &first_token_times,
            tool_call_metrics: &tool_call_metrics,
            provider_info: Some(("anthropic".to_string(), "claude-3".to_string())),
            cache_stats: &cache_stats,
        };

        let stats = snapshot.format();
        
        // Check thinning stats
        assert!(stats.contains("Thinning Events:            3"));
        assert!(stats.contains("Total Chars Saved:       4500")); // 1000+2000+1500
        
        // Check compaction stats
        assert!(stats.contains("Compactions:                1"));
        
        // Check tool call stats
        assert!(stats.contains("Total Tool Calls:           3"));
        assert!(stats.contains("Successful:                 2"));
        assert!(stats.contains("Failed:                     1"));
        
        // Check provider info
        assert!(stats.contains("Provider:          anthropic"));
        assert!(stats.contains("Model:             claude-3"));
        
        // Check cache stats
        assert!(stats.contains("Prompt Cache Statistics"));
        assert!(stats.contains("API Calls:                  5"));
        assert!(stats.contains("Cache Hits:                 3"));
        assert!(stats.contains("Hit Rate:") && stats.contains("60.0%"));
        assert!(stats.contains("Cache Efficiency:"));
    }
}
