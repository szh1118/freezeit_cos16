package io.github.jark006.freezeit.activity;

import android.content.ActivityNotFoundException;
import android.content.Intent;
import android.net.Uri;
import android.os.Bundle;
import android.view.View;
import android.widget.Toast;

import androidx.appcompat.app.AppCompatActivity;

import io.github.jark006.freezeit.R;
import io.github.jark006.freezeit.StaticData;
import io.github.jark006.freezeit.Utils;


public class About extends AppCompatActivity implements View.OnClickListener {

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        setContentView(R.layout.activity_about);

        findViewById(R.id.coolapk_link).setOnClickListener(this);
        findViewById(R.id.github_link).setOnClickListener(this);
        findViewById(R.id.github_app_link).setOnClickListener(this);
        findViewById(R.id.github_project_link).setOnClickListener(this);
        findViewById(R.id.lanzou_link).setOnClickListener(this);

        findViewById(R.id.qq_channel_link).setOnClickListener(this);
        findViewById(R.id.telegram_group).setOnClickListener(this);
        findViewById(R.id.telegram_channel).setOnClickListener(this);
        findViewById(R.id.website_link).setOnClickListener(this);
        findViewById(R.id.tutorial_link).setOnClickListener(this);
        findViewById(R.id.changelog_text).setOnClickListener(this);
        findViewById(R.id.privacy_text).setOnClickListener(this);

        findViewById(R.id.wechat_pay).setOnClickListener(this);
    }

    @Override
    public void onResume() {
        super.onResume();
        findViewById(R.id.container).setBackground(StaticData.getBackgroundDrawable(this));
    }

    private boolean tryStartActivity(Intent intent) {
        try {
            if (intent.resolveActivity(getPackageManager()) == null)
                return false;
            startActivity(intent);
            return true;
        } catch (ActivityNotFoundException | SecurityException e) {
            return false;
        }
    }

    private void openLink(int linkResId) {
        Intent intent = new Intent(Intent.ACTION_VIEW, Uri.parse(getString(linkResId)));
        if (!tryStartActivity(intent))
            Toast.makeText(this, R.string.update_fail, Toast.LENGTH_LONG).show();
    }

    @Override
    public void onClick(View v) {
        int id = v.getId();
        if (id == R.id.coolapk_link) {
            Intent intent = new Intent(Intent.ACTION_VIEW, Uri.parse("coolmarket://u/1212220"));
            intent.setClassName("com.coolapk.market", "com.coolapk.market.view.AppLinkActivity");
            if (!tryStartActivity(intent))
                openLink(R.string.coolapk_link);
        } else if (id == R.id.github_link) {
            openLink(R.string.github_link);
        } else if (id == R.id.github_app_link) {
            openLink(R.string.github_app_link);
        } else if (id == R.id.github_project_link) {
            openLink(R.string.github_project_link);
        } else if (id == R.id.lanzou_link) {
            openLink(R.string.lanzou_link);
        } else if (id == R.id.qq_channel_link) {
            openLink(R.string.qq_channel_link);
        } else if (id == R.id.telegram_group) {
            if (!tryStartActivity(new Intent(Intent.ACTION_VIEW, Uri.parse(getString(R.string.tg_link)))))
                openLink(R.string.tg_https_link);
        } else if (id == R.id.telegram_channel) {
            if (!tryStartActivity(new Intent(Intent.ACTION_VIEW,
                    Uri.parse(getString(R.string.tg_channel_link)))))
                openLink(R.string.tg_channel_https_link);
        } else if (id == R.id.website_link) {
            openLink(R.string.website_link);
        } else if (id == R.id.tutorial_link) {
            openLink(R.string.tutorial_link);
        } else if (id == R.id.changelog_text) {
            openLink(R.string.online_changelog_link);
        } else if (id == R.id.privacy_text) {
            Utils.textDialog(this, R.string.privacy_title, R.string.privacy_content);
        } else if (id == R.id.wechat_pay) {
            Utils.imgDialog(this, R.drawable.img_wechatpay);
        }
    }
}
