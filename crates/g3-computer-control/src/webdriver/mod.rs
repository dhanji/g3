pub mod safari;

use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;

/// WebDriver controller for browser automation
#[async_trait]
pub trait WebDriverController: Send + Sync {
    /// Navigate to a URL
    async fn navigate(&mut self, url: &str) -> Result<()>;
    
    /// Get the current URL
    async fn current_url(&self) -> Result<String>;
    
    /// Get the page title
    async fn title(&self) -> Result<String>;
    
    /// Find an element by CSS selector
    async fn find_element(&mut self, selector: &str) -> Result<WebElement>;
    
    /// Find multiple elements by CSS selector
    async fn find_elements(&mut self, selector: &str) -> Result<Vec<WebElement>>;
    
    /// Execute JavaScript in the browser
    async fn execute_script(&mut self, script: &str, args: Vec<Value>) -> Result<Value>;
    
    /// Get the page source (HTML)
    async fn page_source(&self) -> Result<String>;
    
    /// Take a screenshot and save to path
    async fn screenshot(&mut self, path: &str) -> Result<()>;
    
    /// Close the current window/tab
    async fn close(&mut self) -> Result<()>;
    
    /// Quit the browser session
    async fn quit(self) -> Result<()>;
}

/// Represents a web element in the DOM
pub struct WebElement {
    pub(crate) inner: fantoccini::elements::Element,
}

impl WebElement {
    /// Click the element
    pub async fn click(&mut self) -> Result<()> {
        self.inner.click().await?;
        Ok(())
    }
    
    /// Send keys/text to the element
    pub async fn send_keys(&mut self, text: &str) -> Result<()> {
        self.inner.send_keys(text).await?;
        Ok(())
    }
    
    /// Clear the element's content (for input fields)
    pub async fn clear(&mut self) -> Result<()> {
        self.inner.clear().await?;
        Ok(())
    }
    
    /// Get the element's text content
    pub async fn text(&self) -> Result<String> {
        Ok(self.inner.text().await?)
    }
    
    /// Get an attribute value
    pub async fn attr(&self, name: &str) -> Result<Option<String>> {
        Ok(self.inner.attr(name).await?)
    }
    
    /// Get a property value
    pub async fn prop(&self, name: &str) -> Result<Option<String>> {
        Ok(self.inner.prop(name).await?)
    }
    
    /// Get the element's HTML
    pub async fn html(&self, inner: bool) -> Result<String> {
        Ok(self.inner.html(inner).await?)
    }
    
    /// Check if element is displayed
    pub async fn is_displayed(&self) -> Result<bool> {
        Ok(self.inner.is_displayed().await?)
    }
    
    /// Check if element is enabled
    pub async fn is_enabled(&self) -> Result<bool> {
        Ok(self.inner.is_enabled().await?)
    }
    
    /// Check if element is selected (for checkboxes/radio buttons)
    pub async fn is_selected(&self) -> Result<bool> {
        Ok(self.inner.is_selected().await?)
    }
    
    /// Find a child element by CSS selector
    pub async fn find_element(&mut self, selector: &str) -> Result<WebElement> {
        let elem = self.inner.find(fantoccini::Locator::Css(selector)).await?;
        Ok(WebElement { inner: elem })
    }
    
    /// Find multiple child elements by CSS selector
    pub async fn find_elements(&mut self, selector: &str) -> Result<Vec<WebElement>> {
        let elems = self.inner.find_all(fantoccini::Locator::Css(selector)).await?;
        Ok(elems.into_iter().map(|inner| WebElement { inner }).collect())
    }
}
